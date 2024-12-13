use std::collections::VecDeque;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, Message, MessageRole, ModelDescription,
};
use llmvm_protocol::{NotificationStream, ServiceError};
use reqwest::{Client, Response as HttpResponse};
use serde::Deserialize;

use crate::util::check_status_code;
use crate::{OutsourceError, Result};

const ANTHROPIC_CHAT_COMPLETION_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

const MODEL_KEY: &str = "model";
const SYSTEM_PROMPT_KEY: &str = "system";
const MESSAGES_KEY: &str = "messages";
const MAX_TOKENS_KEY: &str = "max_tokens";
const STREAM_KEY: &str = "stream";

const CONTENT_TEXT_TYPE: &str = "text";
const CONTENT_TEXT_DELTA_TYPE: &str = "text_delta";

const CONTENT_BLOCK_DELTA_EVENT: &str = "content_block_delta";
const CONTENT_BLOCK_START_EVENT: &str = "content_block_start";

const SSE_EVENT_NAME_PREFIX: &str = "event: ";
const SSE_DATA_PREFIX: &str = "data: ";

const AUTH_HEADER: &str = "x-api-key";
const VERSION_HEADER: &str = "anthropic-version";
const VERSION_VALUE: &str = "2023-06-01";

#[derive(Deserialize)]
struct ChatCompletionResponse {
    content: Vec<ChatCompletionContentEntry>,
}

#[derive(Deserialize)]
struct ChatCompletionStreamEvent {
    #[serde(alias = "delta")]
    content_block: ChatCompletionContentEntry,
}

#[derive(Deserialize)]
struct ChatCompletionContentEntry {
    text: Option<String>,
    #[serde(rename = "type")]
    entry_type: String,
}

async fn send_generate_request(
    mut request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
    should_stream: bool,
) -> Result<HttpResponse> {
    let mut body = request.model_parameters.take().unwrap_or_default();

    body.insert(MODEL_KEY.to_string(), model_description.model_name.into());
    body.insert(MAX_TOKENS_KEY.to_string(), request.max_tokens.into());
    let mut messages: Vec<_> = request.thread_messages.take().unwrap_or_default();
    messages.retain(|message| {
        if let MessageRole::System = message.role {
            body.insert(
                SYSTEM_PROMPT_KEY.to_string(),
                message.content.clone().into(),
            );
            false
        } else {
            true
        }
    });
    messages.push(Message {
        role: MessageRole::User,
        content: request.prompt,
    });
    body.insert(MESSAGES_KEY.to_string(), serde_json::to_value(messages)?);

    if should_stream {
        body.insert(STREAM_KEY.to_string(), true.into());
    }

    let client = Client::new();
    let response = client
        .post(ANTHROPIC_CHAT_COMPLETION_ENDPOINT)
        .header(AUTH_HEADER, api_key)
        .header(VERSION_HEADER, VERSION_VALUE)
        .json(&body)
        .send()
        .await?;

    let response = check_status_code(response).await?;

    Ok(response)
}

/// Generate text and return the whole response using the Anthropic API.
pub async fn generate(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
) -> Result<BackendGenerationResponse> {
    let response = send_generate_request(request, model_description, api_key, false).await?;
    let body: ChatCompletionResponse = response.json().await?;
    if body.content.is_empty() {
        return Err(OutsourceError::NoTextInResponse);
    }
    let text_content: Vec<_> = body
        .content
        .into_iter()
        .filter_map(|c| {
            if c.entry_type == CONTENT_TEXT_TYPE {
                Some(c.text.unwrap_or_default())
            } else {
                None
            }
        })
        .collect();

    let response = text_content.as_slice().join("");
    if response.is_empty() {
        return Err(OutsourceError::NoTextInResponse);
    }
    Ok(BackendGenerationResponse { response })
}

fn extract_response_from_stream_event(line_json: &str) -> Result<BackendGenerationResponse> {
    let update: ChatCompletionStreamEvent = serde_json::from_str(line_json)?;
    if update.content_block.entry_type != CONTENT_TEXT_TYPE
        && update.content_block.entry_type != CONTENT_TEXT_DELTA_TYPE
    {
        return Err(OutsourceError::NoTextInResponse);
    }
    let response = update.content_block.text.unwrap_or_default();
    Ok(BackendGenerationResponse { response })
}

/// Request text generation and return an asynchronous stream of generated tokens,
/// using the Anthropic API.
pub async fn generate_stream(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    api_key: &str,
) -> Result<NotificationStream<BackendGenerationResponse>> {
    let response = send_generate_request(request, model_description, api_key, true).await?;
    let mut response_stream = response.bytes_stream();
    Ok(stream! {
        let mut buffer = VecDeque::new();
        let mut skip_next_event_payload = false;
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
                    if line.starts_with(SSE_EVENT_NAME_PREFIX) {
                        let event_name = line[SSE_EVENT_NAME_PREFIX.len()..].trim();
                        if event_name != CONTENT_BLOCK_DELTA_EVENT && event_name != CONTENT_BLOCK_START_EVENT {
                            skip_next_event_payload = true;
                        }
                    } else if line.starts_with(SSE_DATA_PREFIX) {
                        if skip_next_event_payload {
                            skip_next_event_payload = false;
                            continue;
                        }
                        let line_json = line[SSE_DATA_PREFIX.len()..].trim();
                        let result = extract_response_from_stream_event(line_json);
                        yield result.map_err(|e| e.into());
                    }
                }
            }
        }
    }
    .boxed())
}
