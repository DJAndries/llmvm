mod huggingface;
mod openai;
mod util;

use std::str::FromStr;

use llmvm_protocol::{
    async_trait, Backend, BackendGenerationRequest, BackendGenerationResponse, ModelDescription,
    ProtocolError, ProtocolErrorType,
};
use reqwest::StatusCode;
use strum_macros::{Display, EnumString};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, OutsourceError>;

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
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

pub struct OutsourceBackend {
    specified_api_key: Option<String>,
}

impl OutsourceBackend {
    pub fn new(specified_api_key: Option<String>) -> Self {
        Self { specified_api_key }
    }
}

#[async_trait]
impl Backend for OutsourceBackend {
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<BackendGenerationResponse, ProtocolError> {
        async {
            let model_description = ModelDescription::from_str(&request.model)
                .map_err(|_| OutsourceError::ModelDescriptionParse)?;
            let provider =
                Provider::try_from(model_description.provider.as_str()).map_err(|_| {
                    OutsourceError::ProviderNotFound(model_description.provider.to_string())
                })?;
            match provider {
                Provider::OpenAIText => {
                    openai::generate(request, model_description, self.specified_api_key.clone())
                        .await
                }
                Provider::OpenAIChat => {
                    openai::generate(request, model_description, self.specified_api_key.clone())
                        .await
                }
                Provider::HuggingFaceText => {
                    huggingface::generate(
                        request,
                        model_description,
                        self.specified_api_key.clone(),
                    )
                    .await
                }
            }
        }
        .await
        .map_err(|e| e.into())
    }
}
