use std::collections::HashMap;

use llmvm_proto::{BackendGenerationRequest, BackendGenerationResponse};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{check_status_code, get_host_and_api_key};
use crate::{OutsourceError, Result};

const HUGGINGFACE_API_KEY_ENV_KEY: &str = "HUGGINGFACE_API_KEY";
const HUGGINGFACE_API_HOST_ENV_KEY: &str = "HUGGINGFACE_API_HOST";
const DEFAULT_HUGGINGFACE_API_HOST: &str = "https://api-inference.huggingface.co";

const CUSTOM_ENDPOINT_PREFIX: &str = "endpoint=";

const MODELS_ENDPOINT: &str = "models/";

const MAX_TOKENS_KEY: &str = "max_new_tokens";
const RETURN_FULL_KEY: &str = "return_full_text";

#[derive(Serialize)]
struct ModelRequest {
    inputs: String,
    parameters: HashMap<String, Value>,
}

#[derive(Deserialize)]
struct ModelResponse {
    generated_text: String,
}

pub async fn generate(
    mut request: BackendGenerationRequest,
    api_key: Option<String>,
) -> Result<BackendGenerationResponse> {
    let (host, api_key) = get_host_and_api_key(
        HUGGINGFACE_API_KEY_ENV_KEY,
        api_key,
        HUGGINGFACE_API_HOST_ENV_KEY,
        DEFAULT_HUGGINGFACE_API_HOST,
    )?;

    let url = if request.model.starts_with(CUSTOM_ENDPOINT_PREFIX) {
        Url::parse(&request.model[CUSTOM_ENDPOINT_PREFIX.len()..])
            .map_err(|_| OutsourceError::HostURLParse)?
    } else {
        host.join(MODELS_ENDPOINT)
            .unwrap()
            .join(&request.model)
            .unwrap()
    };

    let mut body = ModelRequest {
        inputs: request.prompt,
        parameters: request.model_parameters.take().unwrap_or_default(),
    };

    body.parameters
        .insert(MAX_TOKENS_KEY.to_string(), request.max_tokens.into());
    body.parameters
        .insert(RETURN_FULL_KEY.to_string(), false.into());

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
