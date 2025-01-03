use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use llmvm_protocol::{
    GenerationRequest, GetThreadMessagesRequest, Message, MessageRole, SubscribeToThreadRequest,
    ThreadEvent, ThreadInfo, ToolCall,
};
use llmvm_util::{get_file_path, get_home_dirs, get_project_dir, DirType};
use notify::{RecommendedWatcher, Watcher};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::{
    fs::{self},
    sync::mpsc,
    task::JoinHandle,
    time::sleep,
};

use crate::{
    error::CoreError,
    sessions::{
        create_and_get_session_path, get_session_info, get_session_info_sync,
        start_new_thread_in_session, SessionSubscriberInfo, SESSION_INFO_FILENAME,
    },
    Result,
};

fn thread_path(id: &str) -> Result<PathBuf> {
    get_file_path(DirType::Threads, &format!("{}.json", id), true)
        .ok_or(CoreError::UserHomeNotFound)
}

async fn clean_old_threads_in_dir(directory: &Path, ttl_secs: u64) -> Result<()> {
    let ttl_duration = Duration::from_secs(ttl_secs);
    if let Ok(mut dir_entries) = fs::read_dir(directory).await {
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            if path.extension() != Some("json".as_ref()) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                if let Ok(age) = modified.elapsed() {
                    if age > ttl_duration {
                        fs::remove_file(path).await?;
                    }
                }
            }
        }
    }

    Ok(())
}

pub(super) async fn clean_old_threads(ttl_secs: u64) -> Result<()> {
    if let Some(project_dir) = get_project_dir() {
        clean_old_threads_in_dir(&project_dir.join(DirType::Threads.to_string()), ttl_secs).await?;
    }
    if let Some(home_dirs) = get_home_dirs() {
        clean_old_threads_in_dir(
            &home_dirs.data_dir().join(DirType::Threads.to_string()),
            ttl_secs,
        )
        .await?;
    }
    Ok(())
}

async fn get_thread_infos_in_dir(directory: &Path) -> Result<Vec<ThreadInfo>> {
    let mut result = Vec::new();
    if let Ok(mut dir_entries) = fs::read_dir(directory).await {
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            if path.extension() != Some("json".as_ref()) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                    result.push(ThreadInfo {
                        id: id.to_string(),
                        modified: Some(OffsetDateTime::from(modified).format(&Rfc3339).unwrap()),
                    });
                }
            }
        }
    }
    result.sort_by_cached_key(|ti| ti.modified.clone());
    result.reverse();
    Ok(result)
}

pub(super) async fn get_thread_infos() -> Result<Vec<ThreadInfo>> {
    if let Some(project_dir) = get_project_dir() {
        let result =
            get_thread_infos_in_dir(&project_dir.join(DirType::Threads.to_string())).await?;
        if !result.is_empty() {
            return Ok(result);
        }
    }
    if let Some(home_dirs) = get_home_dirs() {
        return Ok(get_thread_infos_in_dir(
            &home_dirs.data_dir().join(DirType::Threads.to_string()),
        )
        .await?);
    }
    Ok(Vec::new())
}

pub(super) fn new_thread_id() -> String {
    let random_part: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();

    let now = time::OffsetDateTime::now_local()
        .expect("should be able to get current time at local offset");

    format!(
        "{}_{}",
        now.format(
            &time::format_description::parse("[year]-[month]-[day]_[hour]-[minute]").unwrap()
        )
        .unwrap(),
        random_part
    )
}

pub(super) async fn save_thread(thread_id: &str, messages: Vec<Message>) -> Result<()> {
    let thread_path = thread_path(thread_id)?;

    fs::write(
        thread_path,
        serde_json::to_vec(
            &messages
                .iter()
                .filter(|v| !matches!(v.role, MessageRole::System))
                .collect::<Vec<_>>(),
        )?,
    )
    .await?;
    Ok(())
}

pub(super) async fn maybe_save_thread_messages_and_get_thread_id(
    request: &GenerationRequest,
    new_text: String,
    tool_calls: Vec<ToolCall>,
    messages: Option<Vec<Message>>,
    existing_thread_id: Option<String>,
) -> Result<Option<String>> {
    Ok(match messages {
        Some(mut messages) => {
            messages.push(Message {
                client_id: request.client_id.clone(),
                role: MessageRole::Assistant,
                content: new_text,
                tool_calls: Some(tool_calls),
            });
            let thread_id = existing_thread_id.unwrap_or_else(new_thread_id);
            save_thread(&thread_id, messages).await?;
            Some(thread_id)
        }
        None => None,
    })
}

pub(super) async fn get_thread_messages(
    request: &GetThreadMessagesRequest,
) -> Result<(Vec<Message>, String)> {
    let thread_id = if let Some(id) = &request.thread_id {
        id.clone()
    } else if let (Some(session_id), Some(tag)) = (&request.session_id, &request.session_tag) {
        let info = get_session_info(&session_id, &tag).await?;
        let thread_id = match info.thread_id {
            Some(thread_id) => {
                let thread_path = thread_path(&thread_id)?;
                match fs::try_exists(&thread_path).await? {
                    true => Some(thread_id),
                    false => None,
                }
            }
            None => None,
        };
        match thread_id {
            Some(thread_id) => thread_id,
            None => {
                // Create the thread file so future listen on thread calls will succeed
                let new_thread_id = start_new_thread_in_session(session_id, tag).await?;
                save_thread(&new_thread_id, Vec::new()).await?;
                new_thread_id
            }
        }
    } else {
        return Err(CoreError::ThreadNotFound);
    };

    let thread_path = thread_path(&thread_id)?;
    Ok((
        match fs::try_exists(&thread_path).await.unwrap_or_default() {
            true => serde_json::from_slice(&fs::read(&thread_path).await?)?,
            false => Vec::new(),
        },
        thread_id,
    ))
}

fn get_thread_messages_sync(request: &GetThreadMessagesRequest) -> Result<Vec<Message>> {
    let thread_id = if let Some(id) = &request.thread_id {
        id.clone()
    } else if let (Some(group_id), Some(tag)) = (&request.session_id, &request.session_tag) {
        let info = get_session_info_sync(&group_id, &tag)?;
        info.thread_id.ok_or(CoreError::ThreadNotFound)?
    } else {
        return Err(CoreError::ThreadNotFound);
    };

    let thread_path = thread_path(&thread_id)?;
    Ok(match std::fs::metadata(&thread_path).is_ok() {
        true => serde_json::from_slice(&std::fs::read(&thread_path)?)?,
        false => Vec::new(),
    })
}

pub struct SubscriptionHandle {
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    update_task: Option<JoinHandle<()>>,
    subscriber_path: Option<PathBuf>,
}

impl SubscriptionHandle {
    pub fn new(watcher: RecommendedWatcher, subscriber_path: Option<PathBuf>) -> Result<Self> {
        let update_task = subscriber_path.clone().map(|path| {
            tokio::spawn(async move {
                loop {
                    // update last modified time
                    let path_clone = path.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Ok(file) = OpenOptions::new().write(true).open(&path_clone) {
                            let _ = file.set_modified(SystemTime::now());
                        }
                    });

                    // Sleep for 15 seconds
                    sleep(Duration::from_secs(15)).await;
                }
            })
        });

        Ok(Self {
            watcher,
            update_task,
            subscriber_path,
        })
    }
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        if let Some(task) = self.update_task.take() {
            task.abort();
        }
        if let Some(path) = &self.subscriber_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(super) async fn subscribe_to_thread(
    request: SubscribeToThreadRequest,
) -> Result<(mpsc::UnboundedReceiver<ThreadEvent>, SubscriptionHandle)> {
    let (tx, rx) = mpsc::unbounded_channel();

    let (session_path, subscriber_file) = match (&request.session_id, &request.session_tag) {
        (Some(session_id), Some(session_tag)) => {
            let session_path = create_and_get_session_path(session_id, session_tag).await?;

            let subscriber_file = session_path.join(format!("{}.json", request.client_id));
            let subscriber_info = SessionSubscriberInfo {
                client_id: Default::default(),
                tools: request.tools.clone(),
                prompt_parameters: Default::default(),
            };
            fs::write(&subscriber_file, serde_json::to_vec(&subscriber_info)?).await?;

            (Some(session_path), Some(subscriber_file))
        }
        _ => (None, None),
    };

    let client_id = request.client_id;
    let get_msgs_req = GetThreadMessagesRequest {
        thread_id: request.thread_id,
        session_id: request.session_id,
        session_tag: request.session_tag,
    };

    // Get initial messages and store their count
    let (initial_messages, mut thread_id) = get_thread_messages(&get_msgs_req).await?;
    let mut last_count = initial_messages.len();

    let thread_path = thread_path(&thread_id)?;

    // Create a watcher with async configuration
    let mut watcher = notify::recommended_watcher(
        move |res: std::result::Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let event_path = event.paths.first().unwrap();
                let filename = event_path.file_name().unwrap_or_default().to_string_lossy();

                match &event.kind {
                    notify::EventKind::Modify(_) => {
                        if filename == SESSION_INFO_FILENAME && get_msgs_req.session_id.is_some() {
                            if let Ok(info) = get_session_info_sync(
                                &get_msgs_req.session_id.as_ref().unwrap(),
                                &get_msgs_req.session_tag.as_ref().unwrap(),
                            ) {
                                // thread id in session info file does not match the one
                                // we have cached, so there must be a new thread
                                if let Some(updated_thread_id) = info.thread_id {
                                    if updated_thread_id != thread_id {
                                        thread_id = updated_thread_id;
                                        let _ = tx.send(ThreadEvent::NewThread {
                                            thread_id: thread_id.clone(),
                                        });
                                    }
                                }
                            }
                        } else if filename.starts_with(&thread_id) {
                            if let Ok(new_messages) = get_thread_messages_sync(&get_msgs_req) {
                                if new_messages.len() <= last_count {
                                    return;
                                }
                                // Send all new messages since last count
                                for message in new_messages[last_count..].iter().cloned() {
                                    if let Some(msg_client_id) = &message.client_id {
                                        if &client_id == msg_client_id {
                                            continue;
                                        }
                                    }
                                    let _ = tx.send(ThreadEvent::Message { message });
                                }
                                last_count = new_messages.len();
                            }
                        }
                    }
                    notify::EventKind::Create(_) => {
                        if filename != SESSION_INFO_FILENAME && !filename.starts_with(&thread_id) {
                            if let Some(client_id) = filename.strip_suffix(".json") {
                                let _ = tx.send(ThreadEvent::NewSubscriber {
                                    client_id: client_id.to_string(),
                                });
                            }
                        }
                    }
                    _ => (),
                }
            }
        },
    )?;

    watcher.watch(&thread_path, notify::RecursiveMode::NonRecursive)?;
    if let Some(session_info_path) = session_path {
        watcher.watch(&session_info_path, notify::RecursiveMode::NonRecursive)?;
    }

    Ok((rx, SubscriptionHandle::new(watcher, subscriber_file)?))
}
