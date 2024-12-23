//! This module contains protocol types and utilities
//! used for communicating with llmvm core & backends.
//!
//! Uses multilink to communicate with local & remote processes.

#[cfg(any(feature = "http-client", feature = "http-server"))]
pub mod http;
pub mod service;
#[cfg(any(feature = "stdio-client", feature = "stdio-server"))]
pub mod stdio;

pub use async_trait::async_trait;
pub use multilink::*;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fmt::{Display, Formatter},
    str::FromStr,
};
use url::Url;

pub const CHAT_MODEL_PROVIDER_SUFFIX: &str = "-chat";
const CUSTOM_ENDPOINT_PREFIX: &str = "endpoint=";

/// Metadata for a thread.
#[derive(Clone, Serialize, Deserialize)]
pub struct ThreadInfo {
    /// id of the thread.
    pub id: String,
    /// Last modified time of the thread.
    pub modified: Option<String>,
}

/// The actor who presented the message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// For system messages, typically provided as a higher-level prompt for some models.
    System,
    /// For user messages, typically prompted by the user.
    User,
    /// For assistant message, typically generated by the model.
    Assistant,
}

/// A prompt or generated message from a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// ID of the client that sent or received the message.
    pub client_id: Option<String>,
    /// The actor who presented the message.
    pub role: MessageRole,
    /// Text content of the message.
    pub content: String,
}

/// The backend service which the core uses to generate text.
/// Implements a low-level interface for interacting with language models.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Generate text and return the whole response.
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> Result<BackendGenerationResponse, ProtocolError>;

    /// Request text generation and return an asynchronous stream of generated tokens.
    async fn generate_stream(
        &self,
        request: BackendGenerationRequest,
    ) -> Result<NotificationStream<BackendGenerationResponse>, ProtocolError>;
}

/// The core service which frontends use to interact with language models.
/// Manages & uses threads, presets, prompt templates and backend connections to create & send
/// backend requests.
#[async_trait]
pub trait Core: Send + Sync {
    /// Generate text and return the whole response.
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<GenerationResponse, ProtocolError>;

    /// Request text generation and return an asynchronous stream of generated tokens.
    async fn generate_stream(
        &self,
        request: GenerationRequest,
    ) -> Result<NotificationStream<GenerationResponse>, ProtocolError>;

    /// Retrieve information for the last modified thread in the current project
    /// or user data directory.
    async fn get_last_thread_info(&self) -> Result<Option<ThreadInfo>, ProtocolError>;

    /// Retrieve information for all available threads.
    async fn get_all_thread_infos(&self) -> Result<Vec<ThreadInfo>, ProtocolError>;

    /// Retrieve all thread messages for a thread id.
    async fn get_thread_messages(
        &self,
        request: GetThreadMessagesRequest,
    ) -> Result<Vec<Message>, ProtocolError>;

    /// Initialize a new llmvm project in the current directory.
    fn init_project(&self) -> Result<(), ProtocolError>;

    /// Receive notifications for messages on a given thread
    async fn subscribe_to_thread(
        &self,
        request: SubscribeToThreadRequest,
    ) -> Result<NotificationStream<ThreadEvent>, ProtocolError>;

    /// Creates a new thread within a session, returning the thread id
    async fn new_thread_in_session(
        &self,
        request: NewThreadInSessionRequest,
    ) -> Result<String, ProtocolError>;
}

/// Request for language model generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendGenerationRequest {
    /// The id of the language model.
    /// The format of the id is `<backend name>/<model provider name>/<model name>`.
    pub model: String,
    /// The complete prompt to present to the modelv
    pub prompt: String,
    /// Maximum amount of tokens to generate.
    pub max_tokens: u64,
    /// Optional thread messages from previous requests.
    pub thread_messages: Option<Vec<Message>>,
    /// Optional parameters for the model itself. i.e. `temperature`, `top_p`, etc.
    pub model_parameters: Option<Map<String, Value>>,
}

/// Response for language model generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendGenerationResponse {
    /// Generated response from language model.
    pub response: String,
}

/// Parameters used for generation via core service.
/// Can be saved in a preset and/or directly provided within the [`GenerationRequest`].
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GenerationParameters {
    /// The id of the language model.
    /// The format of the id is `<backend name>/<model provider name>/<model name>`.
    pub model: Option<String>,
    /// An optional id for a saved prompt template to use.
    pub prompt_template_id: Option<String>,
    /// Optional text for a custom prompt template. If this is defined
    /// while `prompt_template_id` is defined, then `prompt_template_id` is ignored.
    pub custom_prompt_template: Option<String>,
    /// Maximum amount of tokens to generate.
    pub max_tokens: Option<u64>,
    /// Optional parameters for the model itself. i.e. `temperature`, `top_p`, etc.
    pub model_parameters: Option<Map<String, Value>>,
    /// Parameters for the prompt template.
    pub prompt_parameters: Option<Value>,
}

/// Request for text generation via core service.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationRequest {
    /// An optional id for a saved preset that contains generation parameters.
    pub preset_id: Option<String>,
    /// Model generation parameters. If a preset is provided, present parameter fields
    /// will override the preset values.
    pub parameters: Option<GenerationParameters>,
    /// A custom prompt (not a template) to use for generation.
    pub custom_prompt: Option<String>,
    /// An existing thread id for loadlng previous messages.
    pub existing_thread_id: Option<String>,
    /// ID for a session. To be used in place of `existing_thread_id`.
    pub session_id: Option<String>,
    /// Tag for a session. If `session_id` is provided, this must be provided.
    pub session_tag: Option<String>,
    /// If true, the prompt and response will be saved to the existing thread id
    /// or a new thread.
    pub save_thread: bool,
    /// Random ID of the client to be used over the course of a session.
    pub client_id: Option<String>,
}

/// Response for text generation via core service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationResponse {
    /// The response generated by the language model.
    pub response: String,
    /// Thread id containing the prompt and newly generated response.
    /// Only provided if `save_thread` is set to true in the associated request.
    pub thread_id: Option<String>,
    /// Tool calls made by the model
    pub tool_calls: Vec<ToolCall>,
}

/// Represents a tool call from a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// ID of the tool call, if any
    pub id: Option<String>,
    /// Name of the tool function
    pub name: String,
    /// ID of the client providing the tool
    pub client_id: String,
    /// Arguments of the function
    pub arguments: Value,
}

/// A parsed model id data structure.
pub struct ModelDescription {
    /// Name of the backend to invoke for generation. i.e. `outsource`
    pub backend: String,
    /// Name of the provider of the model. i.e. `openai-chat`
    pub provider: String,
    /// Name of the model. i.e. `gpt-3.5-turbo`
    pub model_name: String,
    /// Custom endpoint (if any)
    pub endpoint: Option<Url>,
}

/// Request to retrieve existing thread messages.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetThreadMessagesRequest {
    /// ID of thread to listen on. If `session_id` and `tag` are not provided, this must be provided.
    pub thread_id: Option<String>,
    /// ID of session to listen on. If `thread_id` is not provided, this must be provided.
    pub session_id: Option<String>,
    /// Tag of thread within the session. If `thread_id` is not provided, this must be provided.
    pub session_tag: Option<String>,
}

/// Request to listen on thread messages
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubscribeToThreadRequest {
    /// ID of thread to subscribe to. If `session_id` and `tag` are not provided, this must be provided.
    pub thread_id: Option<String>,
    /// ID of session to subscribe to. If `thread_id` is not provided, this must be provided.
    pub session_id: Option<String>,
    /// Tag of thread within the session. If `thread_id` is not provided, this must be provided.
    pub session_tag: Option<String>,
    /// Random ID of the client to be used over the course of a session.
    pub client_id: String,
    /// Tools to make available for future messages on the thread.
    pub tools: Option<Vec<Tool>>,
}

/// Request to start a new thread within a given session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThreadInSessionRequest {
    /// ID of session
    pub session_id: String,
    /// Tag of thread within the session
    pub tag: String,
}

/// Event on a given thread
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ThreadEvent {
    /// Notes the start of the session subscription
    Start {
        current_subscribers: Option<Vec<String>>,
    },
    /// New message on thread
    Message { message: Message },
    /// New thread ID created, only dispatched for session listening.
    NewThread { thread_id: String },
    /// New subscriber listening on session
    NewSubscriber { client_id: String },
}

/// Tool definition to be used for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Name of tool
    pub name: String,
    /// Description of tool
    pub description: String,
    /// JSON schema of tool,
    pub input_schema: Value,
    /// Type of tool
    pub tool_type: ToolType,
}

/// Type of tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolType {
    /// Instruct model to call tool via chat text
    Text,
    /// Use native function calling provided by model
    Structured,
}

impl ModelDescription {
    /// Checks if the model is a "chat" model. Currently,
    /// it checks if the provider name ends with `-chat`.
    pub fn is_chat_model(&self) -> bool {
        self.provider.ends_with(CHAT_MODEL_PROVIDER_SUFFIX)
    }
}

impl FromStr for ModelDescription {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let split = s.split("/");
        let tokens: Vec<String> = split.map(|v| v.to_string()).collect();
        if tokens.len() < 3 || tokens[..3].iter().any(|v| v.is_empty()) {
            return Err(());
        }
        let mut tokens_iter = tokens.into_iter();
        let backend = tokens_iter.next().unwrap();
        let provider = tokens_iter.next().unwrap();
        let mut model_name = tokens_iter.collect::<Vec<String>>().join("/");
        let mut endpoint = None;
        if let Some(endpoint_idx) = model_name.rfind(CUSTOM_ENDPOINT_PREFIX) {
            let endpoint_str = &model_name[endpoint_idx + CUSTOM_ENDPOINT_PREFIX.len()..];
            endpoint = Some(Url::parse(endpoint_str).map_err(|_| ())?);
            model_name = model_name[..endpoint_idx].to_string();
            if model_name.ends_with("/") {
                model_name.pop();
            }
        }

        Ok(Self {
            backend,
            provider,
            model_name,
            endpoint,
        })
    }
}

impl Display for ModelDescription {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(endpoint) = self.endpoint.as_ref() {
            if self.model_name.is_empty() {
                write!(
                    f,
                    "{}/{}/{}{}",
                    self.backend, self.provider, CUSTOM_ENDPOINT_PREFIX, endpoint
                )
            } else {
                write!(
                    f,
                    "{}/{}/{}/{}{}",
                    self.backend, self.provider, self.model_name, CUSTOM_ENDPOINT_PREFIX, endpoint
                )
            }
        } else {
            write!(f, "{}/{}/{}", self.backend, self.provider, self.model_name)
        }
    }
}
