use std::collections::VecDeque;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, ModelDescription, NotificationStream,
    ServiceError,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::check_status_code;
use crate::Result;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434/api/generate";

const NUM_PREDICT_KEY: &str = "num_predict";
const TEMPLATE: &str = "{{ .Prompt }}";

#[derive(Serialize)]
struct ModelRequest {
    model: String,
    prompt: Option<String>,
    template: String,
    stream: bool,
    options: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
struct ModelResponse {
    response: String,
}

pub async fn send_request(
    mut request: BackendGenerationRequest,
    model_description: ModelDescription,
    endpoint: Option<&String>,
    stream: bool,
) -> Result<reqwest::Response> {
    let url = endpoint
        .map(|v| v.as_str())
        .unwrap_or_else(|| DEFAULT_ENDPOINT);
    let url = Url::parse(url).unwrap();

    let mut parameters = request.model_parameters.take().unwrap_or_default();

    parameters.insert(NUM_PREDICT_KEY.to_string(), request.max_tokens.into());

    let body = ModelRequest {
        model: model_description.model_name,
        prompt: request.prompt,
        options: request.model_parameters,
        template: TEMPLATE.to_string(),
        stream,
    };

    let client = Client::new();
    let response = client.post(url).json(&body).send().await?;

    check_status_code(response).await
}

/// Generate text and return the whole response using the ollama API.
pub async fn generate(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    endpoint: Option<&String>,
) -> Result<BackendGenerationResponse> {
    let response = send_request(request, model_description, endpoint, false).await?;

    let response: ModelResponse = response.json().await?;

    Ok(BackendGenerationResponse {
        response: Some(response.response),
        tool_call_part: None,
        tool_calls: None,
    })
}

/// Generate text and return a streaming response using the ollama API.
pub async fn generate_stream(
    request: BackendGenerationRequest,
    model_description: ModelDescription,
    endpoint: Option<&String>,
) -> Result<NotificationStream<BackendGenerationResponse>> {
    let response = send_request(request, model_description, endpoint, true).await?;
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
                let result = extract_response_from_stream_event(line_bytes.as_ref());
                yield result.map_err(|e| e.into());
            }
        }
    }
    .boxed())
}

fn extract_response_from_stream_event(line_json: &[u8]) -> Result<BackendGenerationResponse> {
    let reponse: ModelResponse = serde_json::from_slice(line_json)?;
    Ok(BackendGenerationResponse {
        response: Some(reponse.response),
        tool_call_part: None,
        tool_calls: None,
    })
}
