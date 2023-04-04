use std::collections::HashMap;

use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, Message, MessageRole, ModelDescription,
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::util::{check_status_code, get_host_and_api_key};
use crate::{OutsourceError, Result};

const OPENAI_API_KEY_ENV_KEY: &str = "OPENAI_API_KEY";
const OPENAI_API_HOST_ENV_KEY: &str = "OPENAI_API_HOST";
const DEFAULT_OPENAI_API_HOST: &str = "https://api.openai.com";

const CHAT_COMPLETION_ENDPOINT: &str = "/v1/chat/completions";
const COMPLETION_ENDPOINT: &str = "/v1/completions";

const MODEL_KEY: &str = "model";
const PROMPT_KEY: &str = "prompt";
const MESSAGES_KEY: &str = "messages";
const MAX_TOKENS_KEY: &str = "max_tokens";

#[derive(Deserialize)]
struct CompletionChoice {
    text: String,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionChoiceMessage,
}

#[derive(Deserialize)]
struct ChatCompletionChoiceMessage {
    content: String,
}

pub async fn generate(
    mut request: BackendGenerationRequest,
    model_description: ModelDescription,
    specified_api_key: Option<String>,
) -> Result<BackendGenerationResponse> {
    let (host, api_key) = get_host_and_api_key(
        OPENAI_API_KEY_ENV_KEY,
        specified_api_key,
        OPENAI_API_HOST_ENV_KEY,
        DEFAULT_OPENAI_API_HOST,
    )?;

    let is_chat_model = model_description.is_chat_model();
    let endpoint = if is_chat_model {
        CHAT_COMPLETION_ENDPOINT
    } else {
        COMPLETION_ENDPOINT
    };
    let url = host.join(endpoint).unwrap();

    let mut body: HashMap<String, Value> = request.model_parameters.take().unwrap_or_default();

    body.insert(MODEL_KEY.to_string(), model_description.model_name.into());
    body.insert(MAX_TOKENS_KEY.to_string(), request.max_tokens.into());
    if is_chat_model {
        let mut messages: Vec<_> = request.thread_messages.take().unwrap_or_default();
        messages.push(Message {
            role: MessageRole::User,
            content: request.prompt,
        });
        body.insert(MESSAGES_KEY.to_string(), serde_json::to_value(messages)?);
    } else {
        body.insert(PROMPT_KEY.to_string(), request.prompt.into());
    }

    let client = Client::new();
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let response = check_status_code(response).await?;

    let response = if is_chat_model {
        let mut body: ChatCompletionResponse = response.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.message.content
    } else {
        let mut body: CompletionResponse = response.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.text
    };

    Ok(BackendGenerationResponse { response })
}
