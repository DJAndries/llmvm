use std::collections::HashMap;

use llmvm_protocol::{BackendGenerationRequest, BackendGenerationResponse, ModelDescription};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::check_status_code;
use crate::{OutsourceError, Result};

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
    model_description: ModelDescription,
    api_key: &str,
) -> Result<BackendGenerationResponse> {
    let url = if model_description
        .model_name
        .starts_with(CUSTOM_ENDPOINT_PREFIX)
    {
        Url::parse(&model_description.model_name[CUSTOM_ENDPOINT_PREFIX.len()..])
            .map_err(|_| OutsourceError::HostURLParse)?
    } else {
        Url::parse(DEFAULT_HUGGINGFACE_API_HOST)
            .expect("url should parse")
            .join(MODELS_ENDPOINT)
            .unwrap()
            .join(&model_description.model_name)
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
