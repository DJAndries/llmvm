use std::collections::VecDeque;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, Message, MessageRole, ModelDescription,
};
use llmvm_protocol::{NotificationStream, ServiceError};
use reqwest::{Client, Response as HttpResponse, Url};
use serde::Deserialize;

use crate::util::check_status_code;
use crate::{OutsourceError, Result};

const OPENAI_API_HOST: &str = "https://api.openai.com";

const CHAT_COMPLETION_ENDPOINT: &str = "/v1/chat/completions";
const COMPLETION_ENDPOINT: &str = "/v1/completions";

const MODEL_KEY: &str = "model";
const PROMPT_KEY: &str = "prompt";
const MESSAGES_KEY: &str = "messages";
const MAX_TOKENS_KEY: &str = "max_tokens";
const STREAM_KEY: &str = "stream";

const SSE_DATA_PREFIX: &str = "data: ";
const SSE_DONE_MESSAGE: &str = "[DONE]";

#[derive(Deserialize)]
struct CompletionChoice {
    text: Option<String>,
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
struct ChatCompletionStreamResponse {
    choices: Vec<ChatCompletionStreamChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionChoiceMessage,
}

#[derive(Deserialize)]
struct ChatCompletionStreamChoice {
    delta: ChatCompletionChoiceMessage,
}

#[derive(Deserialize)]
struct ChatCompletionChoiceMessage {
    content: Option<String>,
}

async fn send_generate_request(
    mut request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
    is_chat_model: bool,
    should_stream: bool,
) -> Result<HttpResponse> {
    let endpoint = if is_chat_model {
        CHAT_COMPLETION_ENDPOINT
    } else {
        COMPLETION_ENDPOINT
    };
    let url = Url::parse(OPENAI_API_HOST)
        .expect("url should parse")
        .join(endpoint)
        .unwrap();

    let mut body = request.model_parameters.take().unwrap_or_default();

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

    if should_stream {
        body.insert(STREAM_KEY.to_string(), true.into());
    }

    let client = Client::new();
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let response = check_status_code(response).await?;

    Ok(response)
}

pub async fn generate(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
) -> Result<BackendGenerationResponse> {
    let is_chat_model = model_description.is_chat_model();
    let response =
        send_generate_request(request, model_description, api_key, is_chat_model, false).await?;
    let response = if is_chat_model {
        let mut body: ChatCompletionResponse = response.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.message.content
    } else {
        let mut body: CompletionResponse = response.json().await?;
        let choice = body.choices.pop().ok_or(OutsourceError::NoTextInResponse)?;
        choice.text
    }
    .unwrap_or_default();

    Ok(BackendGenerationResponse { response })
}

fn extract_response_from_stream_event(
    line_json: &str,
    is_chat_model: bool,
) -> Result<BackendGenerationResponse> {
    let response = if is_chat_model {
        let mut update: ChatCompletionStreamResponse = serde_json::from_str(line_json)?;
        let choice = update
            .choices
            .pop()
            .ok_or(OutsourceError::NoTextInResponse)?;
        choice.delta.content
    } else {
        let mut update: CompletionResponse = serde_json::from_str(line_json)?;
        let choice = update
            .choices
            .pop()
            .ok_or(OutsourceError::NoTextInResponse)?;
        choice.text
    }
    .unwrap_or_default();
    Ok(BackendGenerationResponse { response })
}

pub async fn generate_stream(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
) -> Result<NotificationStream<BackendGenerationResponse>> {
    let is_chat_model = model_description.is_chat_model();
    let response =
        send_generate_request(request, model_description, api_key, is_chat_model, true).await?;
    let mut response_stream = response.bytes_stream();
    Ok(stream! {
        let mut buffer = VecDeque::new();
        while let Some(bytes_result) = response_stream.next().await {
            match bytes_result {
                Err(e) => {
                    let boxed_e: ServiceError = Box::new(e);
                    yield Err(boxed_e.into());
                    return;
                },
                Ok(bytes) => {
                    buffer.extend(bytes);
                }
            }
            while let Some(linebreak_pos) = buffer.iter().position(|b| b == &b'\n') {
                let line_bytes = buffer.drain(0..(linebreak_pos + 1)).collect::<Vec<_>>();
                if let Ok(line) = std::str::from_utf8(&line_bytes) {
                    if !line.starts_with(SSE_DATA_PREFIX) {
                        continue;
                    }
                    let line_json = &line[SSE_DATA_PREFIX.len()..];
                    if line_json.starts_with(SSE_DONE_MESSAGE) {
                        continue;
                    }
                    let result = extract_response_from_stream_event(line_json, is_chat_model);
                    yield result.map_err(|e| e.into());
                }
            }
        }
    }
    .boxed())
}
