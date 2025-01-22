use std::collections::VecDeque;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, BackendMessage, BackendToolCall,
    MessageRole, ModelDescription,
};
use llmvm_protocol::{NotificationStream, ServiceError};
use reqwest::{Client, Response as HttpResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{error, info};

use crate::util::check_status_code;
use crate::{OutsourceError, Result};

const ANTHROPIC_CHAT_COMPLETION_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

const MODEL_KEY: &str = "model";
const SYSTEM_PROMPT_KEY: &str = "system";
const MESSAGES_KEY: &str = "messages";
const MAX_TOKENS_KEY: &str = "max_tokens";
const STREAM_KEY: &str = "stream";
const TOOLS_KEY: &str = "tools";

const CONTENT_TEXT_TYPE: &str = "text";
const CONTENT_TEXT_DELTA_TYPE: &str = "text_delta";
const INPUT_JSON_DELTA_TYPE: &str = "input_json_delta";
const TOOL_USE_TYPE: &str = "tool_use";

const CONTENT_BLOCK_DELTA_EVENT: &str = "content_block_delta";
const CONTENT_BLOCK_START_EVENT: &str = "content_block_start";
const CONTENT_BLOCK_STOP_EVENT: &str = "content_block_stop";

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
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    partial_json: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AnthropicInputContentEntry {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_result")]
    ToolCallResult {
        tool_use_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicMessageContent {
    Text(String),
    Entries(Vec<AnthropicInputContentEntry>),
}

#[derive(Serialize)]
struct AnthropicInputMessage {
    role: MessageRole,
    content: AnthropicMessageContent,
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

    if let Some(tools) = request.tools {
        body.insert(TOOLS_KEY.to_string(), serde_json::to_value(tools)?);
    }

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
    messages.push(BackendMessage {
        role: MessageRole::User,
        content: request.prompt,
        tool_calls: None,
        tool_call_results: request.tool_call_results,
    });

    let mut anthropic_messages: Vec<AnthropicInputMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        if let Some(results) = message.tool_call_results {
            let mut entries = Vec::with_capacity(results.len());

            for result in results {
                let content = result
                    .result
                    .map(|r| match r.as_str() {
                        Some(result_str) => Ok(result_str.to_string()),
                        None => serde_json::to_string(&r),
                    })
                    .transpose()?;

                entries.push(AnthropicInputContentEntry::ToolCallResult {
                    tool_use_id: result.id,
                    content,
                });
            }

            if let Some(content) = message.content {
                entries.push(AnthropicInputContentEntry::Text { text: content });
            }

            anthropic_messages.push(AnthropicInputMessage {
                role: message.role,
                content: AnthropicMessageContent::Entries(entries),
            });
        } else if let Some(calls) = message.tool_calls {
            let mut entries = Vec::with_capacity(calls.len());

            if let Some(content) = message.content {
                entries.push(AnthropicInputContentEntry::Text { text: content });
            }

            for call in calls {
                entries.push(AnthropicInputContentEntry::ToolCall {
                    id: call.id,
                    name: call.name,
                    input: call.arguments,
                });
            }

            anthropic_messages.push(AnthropicInputMessage {
                role: message.role,
                content: AnthropicMessageContent::Entries(entries),
            });
        } else {
            anthropic_messages.push(AnthropicInputMessage {
                role: message.role,
                content: AnthropicMessageContent::Text(message.content.unwrap_or_default()),
            });
        };
    }

    body.insert(
        MESSAGES_KEY.to_string(),
        serde_json::to_value(anthropic_messages)?,
    );

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
    let mut text_content = Vec::new();
    let mut tool_calls: Option<Vec<_>> = None;
    for entry in body.content {
        match entry.entry_type.as_str() {
            CONTENT_TEXT_TYPE => {
                text_content.push(entry.text.unwrap_or_default());
            }
            TOOL_USE_TYPE => {
                tool_calls.get_or_insert_default().push(BackendToolCall {
                    id: entry.id.ok_or(OutsourceError::InvalidToolCall)?,
                    name: entry.name.ok_or(OutsourceError::InvalidToolCall)?,
                    arguments: entry.input.unwrap_or(Value::Object(Default::default())),
                });
            }
            _ => (),
        }
    }

    let response = text_content.as_slice().join("");
    if response.is_empty() {
        return Err(OutsourceError::NoTextInResponse);
    }
    Ok(BackendGenerationResponse {
        response: Some(response),
        tool_call_part: None,
        tool_calls,
    })
}

fn extract_response_from_stream_event(
    line_json: &str,
    pending_tool_call_input_string: &mut Option<String>,
    tool_calls: &mut Option<Vec<BackendToolCall>>,
) -> Result<BackendGenerationResponse> {
    let update: ChatCompletionStreamEvent = serde_json::from_str(line_json)?;
    Ok(match update.content_block.entry_type.as_str() {
        TOOL_USE_TYPE => {
            let tool_call = BackendToolCall {
                id: update
                    .content_block
                    .id
                    .ok_or(OutsourceError::InvalidToolCall)?,
                name: update
                    .content_block
                    .name
                    .ok_or(OutsourceError::InvalidToolCall)?,
                arguments: update
                    .content_block
                    .input
                    .unwrap_or(Value::Object(Default::default())),
            };
            let mut partial = serde_json::to_string(&tool_call)?;
            partial = partial[..partial.len() - 3].to_string();
            // cut off the end of the json object to create a partial json string
            let _ = pending_tool_call_input_string.insert(String::new());
            tool_calls.get_or_insert_default().push(tool_call);
            BackendGenerationResponse {
                response: None,
                tool_call_part: Some(partial),
                tool_calls: None,
            }
        }
        INPUT_JSON_DELTA_TYPE => {
            let pending_input_string = pending_tool_call_input_string
                .as_mut()
                .ok_or(OutsourceError::InvalidToolCall)?;
            let partial = update
                .content_block
                .partial_json
                .ok_or(OutsourceError::InvalidToolCall)?;
            pending_input_string.push_str(&partial);
            BackendGenerationResponse {
                response: None,
                tool_call_part: Some(partial),
                tool_calls: None,
            }
        }
        CONTENT_TEXT_TYPE | CONTENT_TEXT_DELTA_TYPE => {
            let response = update.content_block.text.unwrap_or_default();
            BackendGenerationResponse {
                response: Some(response),
                tool_call_part: None,
                tool_calls: None,
            }
        }
        _ => {
            return Err(OutsourceError::NoTextInResponse);
        }
    })
}

fn finalize_streamed_tool_call(
    pending_tool_call_input_string: &mut Option<String>,
    tool_calls: Option<&mut Vec<BackendToolCall>>,
) -> Result<BackendGenerationResponse> {
    let latest_tool_call = tool_calls
        .ok_or(OutsourceError::InvalidToolCall)?
        .last_mut()
        .ok_or(OutsourceError::InvalidToolCall)?;
    let input_string = pending_tool_call_input_string.take().unwrap();
    let partial = if input_string.is_empty() {
        "{}}\n".to_string()
    } else {
        latest_tool_call.arguments = serde_json::from_str(&input_string)?;
        "}\n".to_string()
    };
    Ok(BackendGenerationResponse {
        response: None,
        tool_call_part: Some(partial),
        tool_calls: None,
    })
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

        let mut tool_calls = None;
        let mut pending_tool_call_input_string = None;

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
                        if event_name == CONTENT_BLOCK_STOP_EVENT && pending_tool_call_input_string.is_some() {
                            yield finalize_streamed_tool_call(&mut pending_tool_call_input_string, tool_calls.as_mut()).map_err(|e| e.into());
                        }
                        if event_name != CONTENT_BLOCK_DELTA_EVENT && event_name != CONTENT_BLOCK_START_EVENT {
                            skip_next_event_payload = true;
                        }
                    } else if line.starts_with(SSE_DATA_PREFIX) {
                        if skip_next_event_payload {
                            skip_next_event_payload = false;
                            continue;
                        }
                        let line_json = line[SSE_DATA_PREFIX.len()..].trim();
                        let result = extract_response_from_stream_event(line_json, &mut pending_tool_call_input_string, &mut tool_calls);
                        yield result.map_err(|e| e.into());
                    }
                }
            }
        }

        if tool_calls.is_some() {
            yield Ok(BackendGenerationResponse { response: None, tool_call_part: None, tool_calls });
        }
    }
    .boxed())
}
