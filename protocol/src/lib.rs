#[cfg(feature = "stdio")]
pub mod stdio;

#[cfg(feature = "stdio")]
pub use tower;

pub use async_trait::async_trait;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    fmt::{Display, Formatter},
    str::FromStr,
};

pub const CHAT_MODEL_PROVIDER_SUFFIX: &str = "-chat";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProtocolErrorType {
    BadRequest,
    Unauthorized,
    Internal,
}

#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ProtocolError {
    pub error_type: ProtocolErrorType,
    #[source]
    pub error: Box<dyn Error + Send + Sync + 'static>,
}

impl ProtocolError {
    pub fn new(
        error_type: ProtocolErrorType,
        error: Box<dyn Error + Send + Sync + 'static>,
    ) -> Self {
        Self { error_type, error }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Serialize, Deserialize)]
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

    // NOTE: do not eliminate this trait and replace with tower service.
    //       keep it so that other methods can be added.
}

#[async_trait]
pub trait Core: Send + Sync {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResponse, ProtocolError>;
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct BackendGenerationRequest {
    pub model: String,
    pub prompt: String,
    pub max_tokens: u64,
    pub thread_messages: Option<Vec<Message>>,
    pub model_parameters: Option<HashMap<String, Value>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BackendGenerationResponse {
    pub response: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub model: String,
    pub prompt_template_id: Option<String>,
    pub custom_prompt_template: Option<String>,
    pub max_tokens: u64,
    pub model_parameters: Option<HashMap<String, Value>>,
    pub model_parameters_preset_id: Option<String>,
    pub prompt_parameters: Value,
    pub existing_thread_id: Option<u64>,
    pub save_thread: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GenerationResponse {
    pub response: String,
    pub thread_id: Option<u64>,
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
