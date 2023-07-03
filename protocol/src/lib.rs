#[cfg(any(feature = "http-client", feature = "http-server"))]
pub mod http;
pub mod service;
#[cfg(any(feature = "stdio-client", feature = "stdio-server"))]
pub mod stdio;

pub use async_trait::async_trait;
pub use sweetlinks::*;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fmt::{Display, Formatter},
    str::FromStr,
};
use sweetlinks::service::NotificationStream;

pub const CHAT_MODEL_PROVIDER_SUFFIX: &str = "-chat";

#[derive(Clone, Serialize, Deserialize)]
pub struct ThreadInfo {
    pub id: String,
    pub modified: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> Result<BackendGenerationResponse, ProtocolError>;

    async fn generate_stream(
        &self,
        request: BackendGenerationRequest,
    ) -> Result<NotificationStream<BackendGenerationResponse>, ProtocolError>;
}

#[async_trait]
pub trait Core: Send + Sync {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResponse, ProtocolError>;

    async fn generate_stream(
        &self,
        request: GenerationRequest,
    ) -> Result<NotificationStream<GenerationResponse>, ProtocolError>;

    async fn get_last_thread_info(&self) -> Result<Option<ThreadInfo>, ProtocolError>;

    async fn get_all_thread_infos(&self) -> Result<Vec<ThreadInfo>, ProtocolError>;

    async fn get_thread_messages(&self, id: String) -> Result<Vec<Message>, ProtocolError>;

    fn init_project(&self) -> Result<(), ProtocolError>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendGenerationRequest {
    pub model: String,
    pub prompt: String,
    pub max_tokens: u64,
    pub thread_messages: Option<Vec<Message>>,
    pub model_parameters: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendGenerationResponse {
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationParameters {
    pub model: Option<String>,
    pub prompt_template_id: Option<String>,
    pub custom_prompt_template: Option<String>,
    pub max_tokens: Option<u64>,
    pub model_parameters: Option<Map<String, Value>>,
    pub prompt_parameters: Option<Value>,
}

impl Default for GenerationParameters {
    fn default() -> Self {
        Self {
            model: None,
            prompt_template_id: None,
            custom_prompt_template: None,
            max_tokens: Some(2048),
            model_parameters: None,
            prompt_parameters: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub preset_id: Option<String>,
    pub parameters: Option<GenerationParameters>,
    pub custom_prompt: Option<String>,
    pub existing_thread_id: Option<String>,
    pub save_thread: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationResponse {
    pub response: String,
    pub thread_id: Option<String>,
}

pub struct ModelDescription {
    pub backend: String,
    pub provider: String,
    pub model_name: String,
}

impl ModelDescription {
    pub fn is_chat_model(&self) -> bool {
        self.provider.ends_with(CHAT_MODEL_PROVIDER_SUFFIX)
    }
}

impl FromStr for ModelDescription {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let split = s.split("/");
        let tokens: Vec<String> = split.map(|v| v.to_string()).collect();
        if tokens.len() < 3 || tokens.iter().any(|v| v.is_empty()) {
            return Err(());
        }
        let mut tokens_iter = tokens.into_iter();
        Ok(Self {
            backend: tokens_iter.next().unwrap(),
            provider: tokens_iter.next().unwrap(),
            model_name: tokens_iter.collect::<Vec<String>>().join("/"),
        })
    }
}

impl Display for ModelDescription {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.backend, self.provider, self.model_name)
    }
}
