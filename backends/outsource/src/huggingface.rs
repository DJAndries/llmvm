
use std::collections::HashMap;

use llmvm_proto::{BackendGenerationRequest, BackendGenerationResponse, Message, MessageRole};
use reqwest::Client;
use serde::Deserialize;

use crate::util::get_host_and_api_key;
use crate::{OutsourceError, Result};

const HUGGINGFACE_API_KEY_ENV_KEY: &str = "HUGGINGFACE_API_KEY";
const HUGGINGFACE_API_HOST_ENV_KEY: &str = "HUGGINGFACE_API_HOST";
const DEFAULT_HUGGINGFACE_API_HOST: &str = "https://api-inference.huggingface.co";

const CUSTOM_ENDPOINT_PREFIX: &str = "endpoint="; 

const MODELS_ENDPOINT: &str = "models";

const INPUTS_KEY: &str = "model";
const PARAMETERS_KEY: &str = "parameters";
const MAX_TOKENS_KEY: &str = "max_new_tokens";

#[derive(Deserialize)]
struct ModelResponse {
    generated_text: String,
}

pub async fn generate(
    mut request: BackendGenerationRequest,
) -> Result<BackendGenerationResponse> {
    let (host, api_key) = get_host_and_api_key(
        HUGGINGFACE_API_KEY_ENV_KEY,
        HUGGINGFACE_API_HOST_ENV_KEY,
        DEFAULT_HUGGINGFACE_API_HOST,
    )?;

    let url = if request.model.starts_with(ENDPOINT_PREFIX) {
        Url::
    } else {
        host.join(MODEL_KEY).join().unwrap();
    }

    let mut body: HashMap<String, serde_json::Value> =
        request.model_parameters.take().unwrap_or_default();

    body.insert(
        MODEL_KEY.to_string(),
        serde_json::Value::from(request.model),
    );
    body.insert(
        MAX_TOKENS_KEY.to_string(),
        serde_json::Value::from(request.max_tokens),
    );
    if is_chat {
        let mut messages: Vec<_> = request.chat_thread_messages.take().unwrap_or_default();
        messages.push(Message {
            role: MessageRole::User,
            content: request.prompt,
        });
        body.insert(MESSAGES_KEY.to_string(), serde_json::to_value(messages)?);
    } else {
        body.insert(
            PROMPT_KEY.to_string(),
            serde_json::Value::from(request.prompt),
        );
    }

    let client = Client::new();
    let resp = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let response = if is_chat {
        let mut body: ChatCompletionResponse = resp.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.message.content
    } else {
        let mut body: CompletionResponse = resp.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.text
    };

    Ok(BackendGenerationResponse { response })
}