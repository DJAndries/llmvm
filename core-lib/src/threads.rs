use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use llmvm_protocol::{
    GenerationRequest, GetThreadMessagesRequest, ListenOnThreadRequest, Message, MessageRole,
    ThreadEvent, ThreadInfo,
};
use llmvm_util::{get_file_path, get_home_dirs, get_project_dir, DirType};
use notify::{RecommendedWatcher, Watcher};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::{fs, sync::mpsc};

use crate::{error::CoreError, Result};

fn thread_path(id: &str) -> Result<PathBuf> {
    get_file_path(DirType::Threads, &format!("{}.json", id), true)
        .ok_or(CoreError::UserHomeNotFound)
}

const THREAD_GROUPS_FILENAME: &str = "thread_groups.json";

fn thread_groups_path() -> Result<PathBuf> {
    return get_file_path(DirType::MiscData, THREAD_GROUPS_FILENAME, true)
        .ok_or(CoreError::UserHomeNotFound);
}

// map of thread group ids, to maps of tags mapped to thread ids
type ThreadGroupMap = HashMap<String, HashMap<String, String>>;

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

fn new_thread_id() -> String {
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

async fn save_thread(thread_id: &str, messages: Vec<Message>) -> Result<()> {
    let thread_path = thread_path(thread_id)?;

    fs::write(thread_path, serde_json::to_vec(&messages)?).await?;
    Ok(())
}

pub(super) async fn maybe_save_thread_messages_and_get_thread_id(
    request: &GenerationRequest,
    new_text: String,
    messages: Option<Vec<Message>>,
    existing_thread_id: Option<String>,
) -> Result<Option<String>> {
    Ok(match messages {
        Some(mut messages) => {
            messages.push(Message {
                client_id: request.client_id.clone(),
                role: MessageRole::Assistant,
                content: new_text,
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
    } else if let (Some(group_id), Some(tag)) = (&request.thread_group_id, &request.tag) {
        let thread_groups = get_thread_groups().await?;
        let thread_id = match thread_groups.get(group_id).and_then(|group| group.get(tag)) {
            Some(thread_id) => {
                let thread_path = thread_path(&thread_id)?;
                match fs::try_exists(&thread_path).await? {
                    true => Some(thread_id.clone()),
                    false => None,
                }
            }
            None => None,
        };
        match thread_id {
            Some(thread_id) => thread_id,
            None => {
                // Create the thread file so future listen on thread calls will succeed
                let new_thread_id = save_new_thread_in_group(group_id.clone(), tag.clone()).await?;
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
    } else if let (Some(group_id), Some(tag)) = (&request.thread_group_id, &request.tag) {
        let thread_groups = get_thread_groups_sync()?;
        thread_groups
            .get(group_id)
            .and_then(|group| group.get(tag))
            .unwrap_or(&new_thread_id())
            .clone()
    } else {
        return Err(CoreError::ThreadNotFound);
    };

    let thread_path = thread_path(&thread_id)?;
    Ok(match std::fs::metadata(&thread_path).is_ok() {
        true => serde_json::from_slice(&std::fs::read(&thread_path)?)?,
        false => Vec::new(),
    })
}

pub(super) async fn save_new_thread_in_group(group_id: String, tag: String) -> Result<String> {
    let path = thread_groups_path()?;
    let mut map: ThreadGroupMap = match fs::try_exists(&path).await.unwrap_or_default() {
        true => serde_json::from_slice(&fs::read(&path).await?)?,
        false => ThreadGroupMap::new(),
    };

    let thread_id = new_thread_id();
    save_thread(&thread_id, Vec::new()).await?;
    map.entry(group_id.to_string())
        .or_insert_with(HashMap::new)
        .insert(tag.to_string(), thread_id.clone());

    fs::write(path, serde_json::to_vec(&map)?).await?;
    Ok(thread_id)
}

fn get_thread_groups_sync() -> Result<ThreadGroupMap> {
    let path = thread_groups_path()?;
    let map: ThreadGroupMap = match path.exists() {
        true => serde_json::from_slice(&std::fs::read(&path)?)?,
        false => ThreadGroupMap::new(),
    };
    Ok(map)
}

async fn get_thread_groups() -> Result<ThreadGroupMap> {
    let path = thread_groups_path()?;
    let map: ThreadGroupMap = match fs::try_exists(&path).await.unwrap_or_default() {
        true => serde_json::from_slice(&fs::read(&path).await?)?,
        false => ThreadGroupMap::new(),
    };
    Ok(map)
}

pub(super) async fn listen_on_thread(
    request: ListenOnThreadRequest,
) -> Result<(mpsc::UnboundedReceiver<ThreadEvent>, RecommendedWatcher)> {
    let (tx, rx) = mpsc::unbounded_channel();

    let using_tag = request.tag.is_some();
    let client_id = request.client_id;
    let get_msgs_req = GetThreadMessagesRequest {
        thread_id: request.thread_id,
        thread_group_id: request.thread_group_id,
        tag: request.tag,
    };

    let thread_groups_path = thread_groups_path()?;

    // Get initial messages and store their count
    let (initial_messages, thread_id) = get_thread_messages(&get_msgs_req).await?;
    let mut last_count = initial_messages.len();

    let thread_path = thread_path(&thread_id)?;

    // Create a watcher with async configuration
    let mut watcher = notify::recommended_watcher(
        move |res: std::result::Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                if let notify::EventKind::Modify(_) = &event.kind {
                    let event_path = event.paths.first().unwrap();
                    if event_path.file_name().unwrap_or_default() == THREAD_GROUPS_FILENAME
                        && get_msgs_req.thread_group_id.is_some()
                    {
                        if let Ok(thread_groups) = get_thread_groups_sync() {
                            if let Some(new_thread_id) = thread_groups
                                .get(get_msgs_req.thread_group_id.as_ref().unwrap())
                                .and_then(|group| group.get(get_msgs_req.tag.as_ref().unwrap()))
                            {
                                // thread id in group file does not match the one
                                // we have cached, so there must be a new thread
                                if new_thread_id != &thread_id {
                                    let _ = tx.send(ThreadEvent::NewThread {
                                        thread_id: new_thread_id.clone(),
                                    });
                                }
                            }
                        }
                    } else {
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
            }
        },
    )?;

    watcher.watch(&thread_path, notify::RecursiveMode::NonRecursive)?;
    if using_tag {
        watcher.watch(&thread_groups_path, notify::RecursiveMode::NonRecursive)?;
    }

    Ok((rx, watcher))
}
