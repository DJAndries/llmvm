#[cfg(feature = "tower")]
pub mod services;
#[cfg(feature = "tower")]
pub use tower;

#[cfg(feature = "jsonrpc")]
pub mod jsonrpc;
#[cfg(feature = "stdio")]
pub mod stdio;

#[cfg(any(feature = "http-client", feature = "http-server"))]
pub mod http;

pub use async_trait::async_trait;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    error::Error,
    fmt::{Display, Formatter},
    str::FromStr,
};

pub const COMMAND_TIMEOUT_SECS: u64 = 900;
pub const CHAT_MODEL_PROVIDER_SUFFIX: &str = "-chat";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProtocolErrorType {
    NotFound,
    HttpMethodNotAllowed,
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

impl From<Box<dyn Error + Send + Sync + 'static>> for ProtocolError {
    fn from(error: Box<dyn Error + Send + Sync + 'static>) -> Self {
        match error.downcast::<Self>() {
            Ok(e) => *e,
            Err(e) => ProtocolError::new(ProtocolErrorType::Internal, e),
        }
    }
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

    // NOTE: do not eliminate this trait and replace with tower service.
    //       keep it so that other methods can be added.
}

#[async_trait]
pub trait Core: Send + Sync {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResponse, ProtocolError>;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationParameters {
    pub model: Option<String>,
    pub prompt_template_id: Option<String>,
    pub custom_prompt_template: Option<String>,
    pub max_tokens: Option<u64>,
    pub model_parameters: Option<Map<String, Value>>,
    pub prompt_parameters: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub preset_id: Option<String>,
    pub parameters: Option<GenerationParameters>,
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

// TODO: move back into http::config module which will be avail without features
// TODO: put server and client in separate modules, get rid of cfg for each block
#[derive(Deserialize)]
pub struct HttpServerConfig {
    pub port: u16,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self { port: 8080 }
    }
}

#[derive(Deserialize)]
pub struct HttpClientConfig {
    pub base_url: String,
}
