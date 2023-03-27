use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Serialize, Deserialize)]
pub struct BackendGenerationRequest {
    pub model: String,
    pub prompt: String,
    pub max_tokens: u64,
    pub chat_thread_messages: Option<Vec<Message>>,
    pub model_parameters: Option<HashMap<String, Value>>,
}

#[derive(Serialize, Deserialize)]
pub struct BackendGenerationResponse {
    pub response: String,
}

#[derive(Serialize, Deserialize)]
pub struct GenerationRequest {
    pub backend: String,
    pub model: String,
    pub prompt_id: Option<String>,
    pub custom_prompt: Option<String>,
    pub prompt_parameters: Option<HashMap<String, String>>,
    pub existing_thread_id: Option<i64>,
    pub save_thread: bool,
}

#[derive(Serialize, Deserialize)]
pub struct GenerationResponse {
    pub response: String,
    pub thread_id: Option<i64>,
}
