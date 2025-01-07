use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use llmvm_protocol::{SessionPromptParameter, Tool};
use llmvm_util::{get_file_path, DirType};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::fs::{self, DirEntry};

use crate::error::CoreError;
use crate::threads::{new_thread_id, save_thread};
use crate::Result;

pub(super) const SESSION_INFO_FILENAME: &str = "info.json";
const SESSION_INFO_EXPIRY_SECS: u64 = 60 * 5;

#[derive(Serialize, Deserialize, Default)]
pub(super) struct SessionInfo {
    pub thread_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SessionSubscriberInfo {
    #[serde(skip)]
    pub client_id: String,
    pub tools: Option<Vec<Tool>>,
    pub prompt_parameters: HashMap<String, SessionPromptParameter>,
}

const MAX_SUB_AGE_SECS: u64 = 30;

pub(super) async fn create_and_get_session_path(id: &str, tag: &str) -> Result<PathBuf> {
    let path = session_path(id, tag)?;
    fs::create_dir_all(&path).await?;
    Ok(path)
}

fn session_path(id: &str, tag: &str) -> Result<PathBuf> {
    get_file_path(
        DirType::Sessions,
        &url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>(),
        true,
    )
    .map(|p| p.join(tag))
    .ok_or(CoreError::UserHomeNotFound)
}

async fn is_subscriber_alive(dir_entry: &DirEntry, now: &SystemTime) -> Result<bool> {
    let age = now
        .duration_since(dir_entry.metadata().await?.modified()?)
        .unwrap();
    // Subscribers will actively update their last modified time periodically
    // If the subscriber file has not been updated in a while, then ignore them and assume they're no longer active
    let expired = age.as_secs() >= MAX_SUB_AGE_SECS;
    if expired {
        fs::remove_file(dir_entry.path()).await?;
    }
    Ok(!expired)
}

pub(super) async fn get_session_subscribers(
    session_id: &str,
    tag: &str,
) -> Result<Vec<SessionSubscriberInfo>> {
    let session_path = create_and_get_session_path(session_id, tag).await?;
    let mut subscribers = Vec::new();
    let now = SystemTime::now();

    let mut entries = fs::read_dir(&session_path).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();

        if !filename.ends_with(".json") || filename == SESSION_INFO_FILENAME {
            continue;
        }

        if let Some(client_id) = filename.strip_suffix(".json") {
            if !is_subscriber_alive(&entry, &now).await? {
                continue;
            }
            let mut info =
                serde_json::from_slice::<SessionSubscriberInfo>(&fs::read(entry.path()).await?)?;
            info.client_id = client_id.to_string();
            subscribers.push(info);
        }
    }

    Ok(subscribers)
}

pub(super) async fn start_new_thread_in_session(session_id: &str, tag: &str) -> Result<String> {
    let path = create_and_get_session_path(&session_id, &tag)
        .await?
        .join(SESSION_INFO_FILENAME);

    let thread_id = new_thread_id();
    let info = match fs::try_exists(&path).await? {
        true => {
            let mut existing_info: SessionInfo = serde_json::from_slice(&fs::read(&path).await?)?;
            existing_info.thread_id = Some(thread_id.clone());
            existing_info
        }
        false => SessionInfo {
            thread_id: Some(thread_id.clone()),
        },
    };

    save_thread(&thread_id, Vec::new()).await?;

    fs::write(path, serde_json::to_vec(&info)?).await?;
    Ok(thread_id)
}

pub(super) async fn store_session_prompt_parameter(
    session_id: &str,
    tag: &str,
    client_id: &str,
    param_name: String,
    param_info: Option<SessionPromptParameter>,
) -> Result<()> {
    let path = create_and_get_session_path(&session_id, &tag)
        .await?
        .join(format!("{}.json", client_id));

    let bytes = fs::read(&path).await?;
    let mut info: SessionSubscriberInfo = serde_json::from_slice(&bytes)?;

    match param_info {
        Some(param_info) => info.prompt_parameters.insert(param_name, param_info),
        None => info.prompt_parameters.remove(&param_name),
    };

    fs::write(path, serde_json::to_vec(&info)?).await?;
    Ok(())
}

pub(super) async fn get_session_prompt_parameters(
    subscribers: &[SessionSubscriberInfo],
) -> Result<Map<String, Value>> {
    let mut param_map = Map::new();

    for subscriber in subscribers {
        // Process each parameter
        for (key, param) in &subscriber.prompt_parameters {
            let parts: Vec<&str> = key.split('.').collect();
            let mut current_map = &mut param_map;

            // Handle nested paths
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    // Last part - insert the actual value
                    current_map.insert(part.to_string(), param.value.clone());
                } else {
                    // Create or get nested map
                    current_map = current_map
                        .entry(part.to_string())
                        .or_insert_with(|| Value::Object(Map::new()))
                        .as_object_mut()
                        .unwrap();
                }
            }
        }
    }

    Ok(param_map)
}

pub(super) fn get_session_info_sync(session_id: &str, tag: &str) -> Result<SessionInfo> {
    let path = session_path(session_id, tag)?.join(SESSION_INFO_FILENAME);
    let info: SessionInfo = match path.exists() {
        true => serde_json::from_slice(&std::fs::read(&path)?)?,
        false => Default::default(),
    };
    Ok(info)
}

pub(super) async fn get_session_info(session_id: &str, tag: &str) -> Result<SessionInfo> {
    let session_path = create_and_get_session_path(session_id, tag).await?;
    let info_path = session_path.join(SESSION_INFO_FILENAME);

    if !fs::try_exists(&info_path).await.unwrap_or_default() {
        return Ok(Default::default());
    }

    let mut has_subscribers = false;
    let mut dir = fs::read_dir(session_path).await?;
    let now = SystemTime::now();
    while let Some(entry) = dir.next_entry().await? {
        if entry.path() != info_path && is_subscriber_alive(&entry, &now).await? {
            has_subscribers = true;
            break;
        }
    }

    if !has_subscribers {
        let metadata = fs::metadata(&info_path).await?;
        let modified = metadata.modified()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if age > Duration::from_secs(SESSION_INFO_EXPIRY_SECS) {
            return Ok(Default::default());
        }
    }

    let info_path_clone = info_path.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now();
        let _ = std::fs::File::open(&info_path_clone).and_then(|f| f.set_modified(now));
    })
    .await;
    Ok(serde_json::from_slice(&fs::read(&info_path).await?)?)
}
