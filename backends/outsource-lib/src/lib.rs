//! [llmvm](https://github.com/djandries/llmvm) backend which forwards generation requests
//! to known hosted providers.
//!
//! Currently supported providers:
//! - OpenAI (text and chat interface)
//! - Hugging Face (text interface)

mod huggingface;
mod ollama;
mod openai;
mod util;

use std::str::FromStr;

use futures::{stream::once, StreamExt};
use llmvm_protocol::{
    async_trait, error::ProtocolErrorType, Backend, BackendGenerationRequest,
    BackendGenerationResponse, ConfigExampleSnippet, ModelDescription, NotificationStream,
    ProtocolError,
};
use reqwest::StatusCode;
use serde::Deserialize;
use strum_macros::{Display, EnumString};
use thiserror::Error;
use util::get_api_key;

pub type Result<T> = std::result::Result<T, OutsourceError>;

/// Error enum containing all possible backend errors.
#[derive(Debug, Error)]
pub enum OutsourceError {
    #[error("provider for model not found, assumed provider name is '{0}'")]
    ProviderNotFound(String),
    #[error("api key not defined")]
    APIKeyNotDefined,
    #[error("could not parse api host as url")]
    HostURLParse,
    #[error("http request error: {0}")]
    HttpRequestError(#[from] reqwest::Error),
    #[error("bad http status code: {status} body: {body}")]
    BadHttpStatusCode { status: StatusCode, body: String },
    #[error("json serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("no text in response")]
    NoTextInResponse,
    #[error("failed to parse model name")]
    ModelDescriptionParse,
    #[error("model parameters should be object")]
    ModelParamsNotObject,
}

#[derive(Display, EnumString)]
#[strum(ascii_case_insensitive)]
enum Provider {
    #[strum(serialize = "openai-text")]
    OpenAIText,
    #[strum(serialize = "openai-chat")]
    OpenAIChat,
    #[strum(serialize = "huggingface-text")]
    HuggingFaceText,
    #[strum(serialize = "ollama-text")]
    OllamaText,
}

impl Into<ProtocolError> for OutsourceError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            OutsourceError::ProviderNotFound(_) => ProtocolErrorType::BadRequest,
            OutsourceError::APIKeyNotDefined => ProtocolErrorType::BadRequest,
            OutsourceError::HostURLParse => ProtocolErrorType::BadRequest,
            OutsourceError::HttpRequestError(_) => ProtocolErrorType::Internal,
            OutsourceError::BadHttpStatusCode { .. } => ProtocolErrorType::Internal,
            OutsourceError::Serialization(_) => ProtocolErrorType::Internal,
            OutsourceError::NoTextInResponse => ProtocolErrorType::Internal,
            OutsourceError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            OutsourceError::ModelParamsNotObject => ProtocolErrorType::BadRequest,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

/// Configuration structure for the backend.
#[derive(Deserialize)]
pub struct OutsourceConfig {
    pub openai_api_key: Option<String>,
    pub huggingface_api_key: Option<String>,
    pub ollama_endpoint: Option<String>,
}

impl ConfigExampleSnippet for OutsourceConfig {
    fn config_example_snippet() -> String {
        r#"# API key for OpenAI
# openai_api_key = ""

# API key for Hugging Face
# huggingface_api_key = ""

# Endpoint for ollama (defaults to http://127.0.0.1:11434/api/generate)
# ollama_endpoint = """#
            .into()
    }
}

/// An llmvm backend that forwards requests to known hosted providers.
pub struct OutsourceBackend {
    config: OutsourceConfig,
}

impl OutsourceBackend {
    pub fn new(config: OutsourceConfig) -> Self {
        Self { config }
    }

    fn get_model_description_and_provider(
        request: &BackendGenerationRequest,
    ) -> Result<(ModelDescription, Provider)> {
        let model_description = ModelDescription::from_str(&request.model)
            .map_err(|_| OutsourceError::ModelDescriptionParse)?;
        let provider = Provider::try_from(model_description.provider.as_str()).map_err(|_| {
            OutsourceError::ProviderNotFound(model_description.provider.to_string())
        })?;
        Ok((model_description, provider))
    }
}

#[async_trait]
impl Backend for OutsourceBackend {
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<BackendGenerationResponse, ProtocolError> {
        async {
            let (model_description, provider) = Self::get_model_description_and_provider(&request)?;
            match provider {
                Provider::OpenAIText | Provider::OpenAIChat => {
                    openai::generate(
                        request,
                        model_description,
                        get_api_key(self.config.openai_api_key.as_ref())?,
                    )
                    .await
                }
                Provider::HuggingFaceText => {
                    huggingface::generate(
                        request,
                        model_description,
                        get_api_key(self.config.huggingface_api_key.as_ref())?,
                    )
                    .await
                }
                Provider::OllamaText => {
                    ollama::generate(
                        request,
                        model_description,
                        self.config.ollama_endpoint.as_ref(),
                    )
                    .await
                }
            }
        }
        .await
        .map_err(|e| e.into())
    }

    async fn generate_stream(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<NotificationStream<BackendGenerationResponse>, ProtocolError> {
        async {
            let (model_description, provider) = Self::get_model_description_and_provider(&request)?;
            match provider {
                Provider::OpenAIText | Provider::OpenAIChat => {
                    openai::generate_stream(
                        request,
                        model_description,
                        get_api_key(self.config.openai_api_key.as_ref())?,
                    )
                    .await
                }
                Provider::HuggingFaceText => {
                    let api_key =
                        get_api_key(self.config.huggingface_api_key.as_ref())?.to_string();
                    Ok(once(async move {
                        huggingface::generate(request, model_description, &api_key)
                            .await
                            .map_err(|e| e.into())
                    })
                    .boxed())
                }
                Provider::OllamaText => {
                    ollama::generate_stream(
                        request,
                        model_description,
                        self.config.ollama_endpoint.as_ref(),
                    )
                    .await
                }
            }
        }
        .await
        .map_err(|e| e.into())
    }
}
