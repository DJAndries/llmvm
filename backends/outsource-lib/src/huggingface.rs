use llmvm_protocol::{BackendGenerationRequest, BackendGenerationResponse, ModelDescription};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::check_status_code;
use crate::Result;

const DEFAULT_HUGGINGFACE_API_HOST: &str = "https://api-inference.huggingface.co";

const MODELS_ENDPOINT: &str = "models/";

const MAX_TOKENS_KEY: &str = "max_new_tokens";
const RETURN_FULL_KEY: &str = "return_full_text";

#[derive(Serialize)]
struct ModelRequest {
    inputs: String,
    parameters: Map<String, Value>,
}

#[derive(Deserialize)]
struct ModelResponse {
    generated_text: String,
}

/// Generate text and return the whole response using the Hugging Face API.
///
/// Custom hosted endpoints may be used by supplying the prefix `endpoint=`, followed by the endpoint
/// URL in the model name component of the model id. For example, the
/// model ID could be `outsource/huggingface-text/endpoint=https://yourendpointhere`.
pub async fn generate(
    mut request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
) -> Result<BackendGenerationResponse> {
    let url = if model_description.endpoint.is_some() {
        model_description.endpoint.unwrap()
    } else {
        Url::parse(DEFAULT_HUGGINGFACE_API_HOST)
            .expect("url should parse")
            .join(MODELS_ENDPOINT)
            .unwrap()
            .join(&model_description.model_name)
            .unwrap()
    };

    let mut parameters = request.model_parameters.take().unwrap_or_default();

    parameters.insert(MAX_TOKENS_KEY.to_string(), request.max_tokens.into());
    parameters.insert(RETURN_FULL_KEY.to_string(), false.into());

    let body = ModelRequest {
        inputs: request.prompt,
        parameters,
    };

    let client = Client::new();
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let response = check_status_code(response).await?;

    let mut response: Vec<ModelResponse> = response.json().await?;

    Ok(BackendGenerationResponse {
        response: response.pop().unwrap().generated_text,
    })
}
