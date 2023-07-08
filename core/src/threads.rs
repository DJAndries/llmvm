use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use llmvm_protocol::{GenerationRequest, Message, MessageRole, ThreadInfo};
use llmvm_util::{get_file_path, get_home_dirs, get_project_dir, DirType};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::fs;

use crate::{error::CoreError, Result};

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
                        modified: OffsetDateTime::from(modified).format(&Rfc3339).unwrap(),
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
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
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
) -> Result<Option<String>> {
    Ok(match messages {
        Some(mut messages) => {
            messages.push(Message {
                role: MessageRole::Assistant,
                content: new_text,
            });
            let thread_id = request
                .existing_thread_id
                .clone()
                .unwrap_or_else(new_thread_id);
            save_thread(&thread_id, messages).await?;
            Some(thread_id)
        }
        None => None,
    })
}

pub(super) async fn get_thread_messages(thread_id: &str) -> Result<Vec<Message>> {
    let thread_path = thread_path(thread_id)?;
    Ok(
        match fs::try_exists(&thread_path).await.unwrap_or_default() {
            true => serde_json::from_slice(&fs::read(&thread_path).await?)?,
            false => Vec::new(),
        },
    )
}
