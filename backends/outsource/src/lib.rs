// mod huggingface;
mod openai;
mod util;

use llmvm_proto::{BackendGenerationRequest, BackendGenerationResponse};
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
    #[error("web request error: {0}")]
    WebRequestError(#[from] reqwest::Error),
    #[error("json serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("no text in response")]
    NoTextInResponse,
}

#[derive(Display, EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum Provider {
    OpenAI,
    #[strum(serialize = "openai-chat")]
    OpenAIChat,
    HuggingFace,
}

fn extract_provider_from_model_name(request: &mut BackendGenerationRequest) -> Result<Provider> {
    let mut model_name_split = request.model.split("/");
    let provider_name = model_name_split.next().unwrap();
    let provider = Provider::try_from(provider_name)
        .map_err(|_| OutsourceError::ProviderNotFound(provider_name.to_string()))?;
    request.model = model_name_split.collect::<Vec<&str>>().join("/");
    Ok(provider)
}

pub async fn generate(mut request: BackendGenerationRequest) -> Result<BackendGenerationResponse> {
    let provider = extract_provider_from_model_name(&mut request)?;
    match provider {
        Provider::OpenAI => openai::generate(request, false).await,
        Provider::OpenAIChat => openai::generate(request, true).await,
        Provider::HuggingFace => openai::generate(request, false).await,
    }
}
