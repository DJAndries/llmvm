pub use multilink::stdio::*;

use multilink::{
    jsonrpc::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse},
    util::parse_from_value,
    ProtocolError,
};
use serde_json::Value;

use crate::service::{BackendRequest, BackendResponse, CoreRequest, CoreResponse};

const GENERATION_METHOD: &str = "generation";
const GENERATION_STREAM_METHOD: &str = "generation_stream";
const INIT_PROJECT_METHOD: &str = "init_project";
const GET_LAST_THREAD_INFO_METHOD: &str = "get_last_thread_info";
const GET_ALL_THREAD_INFOS_METHOD: &str = "get_all_thread_infos";
const GET_THREAD_MESSAGES_METHOD: &str = "get_thread_messages";
const LISTEN_ON_THREAD_METHOD: &str = "listen_on_thread";
const NEW_THREAD_IN_SESSION_METHOD: &str = "new_thread_in_session";
const STORE_SESSION_PROMPT_PARAMETER_METHOD: &str = "store_session_prompt_parameter";

impl RequestJsonRpcConvert<CoreRequest> for CoreRequest {
    fn from_jsonrpc_request(value: JsonRpcRequest) -> Result<Option<Self>, ProtocolError> {
        Ok(Some(match value.method.as_str() {
            GENERATION_METHOD => CoreRequest::Generation(value.parse_params()?),
            GENERATION_STREAM_METHOD => CoreRequest::GenerationStream(value.parse_params()?),
            INIT_PROJECT_METHOD => CoreRequest::InitProject,
            GET_LAST_THREAD_INFO_METHOD => CoreRequest::GetLastThreadInfo,
            GET_ALL_THREAD_INFOS_METHOD => CoreRequest::GetAllThreadInfos,
            GET_THREAD_MESSAGES_METHOD => CoreRequest::GetThreadMessages(value.parse_params()?),
            LISTEN_ON_THREAD_METHOD => CoreRequest::SubscribeToThread(value.parse_params()?),
            NEW_THREAD_IN_SESSION_METHOD => CoreRequest::NewThreadInSession(value.parse_params()?),
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_request(&self) -> JsonRpcRequest {
        let (method, params) = match self {
            CoreRequest::Generation(request) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::GenerationStream(request) => (
                GENERATION_STREAM_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::InitProject => (INIT_PROJECT_METHOD, None),
            CoreRequest::GetLastThreadInfo => (GET_LAST_THREAD_INFO_METHOD, None),
            CoreRequest::GetAllThreadInfos => (GET_ALL_THREAD_INFOS_METHOD, None),
            CoreRequest::GetThreadMessages(request) => (
                GET_THREAD_MESSAGES_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::SubscribeToThread(request) => (
                LISTEN_ON_THREAD_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::NewThreadInSession(request) => (
                NEW_THREAD_IN_SESSION_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::StoreSessionPromptParameter(request) => (
                STORE_SESSION_PROMPT_PARAMETER_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
        };
        JsonRpcRequest::new(method.to_string(), params)
    }
}

impl ResponseJsonRpcConvert<CoreRequest, CoreResponse> for CoreResponse {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &CoreRequest,
    ) -> Result<Option<Self>, ProtocolError> {
        match value {
            JsonRpcMessage::Response(resp) => {
                let result = resp.get_result()?;
                Ok(Some(match original_request {
                    CoreRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                    CoreRequest::GetLastThreadInfo => {
                        Self::GetLastThreadInfo(parse_from_value(result)?)
                    }
                    CoreRequest::GetAllThreadInfos => {
                        Self::GetAllThreadInfos(parse_from_value(result)?)
                    }
                    CoreRequest::GetThreadMessages { .. } => {
                        Self::GetThreadMessages(parse_from_value(result)?)
                    }
                    CoreRequest::InitProject => Self::InitProject,
                    CoreRequest::NewThreadInSession(_) => {
                        Self::NewThreadInSession(parse_from_value(result)?)
                    }
                    CoreRequest::StoreSessionPromptParameter(_) => {
                        Self::StoreSessionPromptParameter
                    }
                    _ => return Ok(None),
                }))
            }
            JsonRpcMessage::Notification(resp) => {
                let result = resp.get_result()?;
                Ok(Some(match original_request {
                    CoreRequest::GenerationStream(_) => {
                        Self::GenerationStream(parse_from_value(result)?)
                    }
                    CoreRequest::SubscribeToThread { .. } => {
                        Self::ListenOnThread(parse_from_value(result)?)
                    }
                    _ => return Ok(None),
                }))
            }
            _ => Ok(None),
        }
    }

    fn into_jsonrpc_message(response: CoreResponse, id: Value) -> JsonRpcMessage {
        let mut is_notification = false;
        let result = Ok(match response {
            CoreResponse::Generation(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::GenerationStream(response) => {
                is_notification = true;
                serde_json::to_value(response).unwrap()
            }
            CoreResponse::GetLastThreadInfo(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::GetAllThreadInfos(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::GetThreadMessages(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::InitProject => Value::Null,
            CoreResponse::ListenOnThread(response) => {
                is_notification = true;
                serde_json::to_value(response).unwrap()
            }
            CoreResponse::NewThreadInSession(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::StoreSessionPromptParameter => Value::Null,
        });
        match is_notification {
            true => JsonRpcNotification::new_with_result_params(result, id.to_string()).into(),
            false => JsonRpcResponse::new(result, id).into(),
        }
    }
}

impl RequestJsonRpcConvert<BackendRequest> for BackendRequest {
    fn from_jsonrpc_request(value: JsonRpcRequest) -> Result<Option<Self>, ProtocolError> {
        Ok(Some(match value.method.as_str() {
            GENERATION_METHOD => BackendRequest::Generation(value.parse_params()?),
            GENERATION_STREAM_METHOD => BackendRequest::GenerationStream(value.parse_params()?),
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_request(&self) -> JsonRpcRequest {
        let (method, params) = match &self {
            BackendRequest::Generation(generation_response) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(generation_response).unwrap()),
            ),
            BackendRequest::GenerationStream(request) => (
                GENERATION_STREAM_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
        };
        JsonRpcRequest::new(method.to_string(), params)
    }
}

impl ResponseJsonRpcConvert<BackendRequest, BackendResponse> for BackendResponse {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &BackendRequest,
    ) -> Result<Option<Self>, ProtocolError> {
        Ok(Some(match value {
            JsonRpcMessage::Response(resp) => {
                let result = resp.get_result()?;
                match original_request {
                    BackendRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                    _ => return Ok(None),
                }
            }
            JsonRpcMessage::Notification(resp) => {
                let result = resp.get_result()?;
                match original_request {
                    BackendRequest::GenerationStream(_) => {
                        Self::GenerationStream(parse_from_value(result)?)
                    }
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_message(response: BackendResponse, id: Value) -> JsonRpcMessage {
        let mut is_notification = false;
        let result = Ok(match response {
            BackendResponse::Generation(response) => serde_json::to_value(response).unwrap(),
            BackendResponse::GenerationStream(response) => {
                is_notification = true;
                serde_json::to_value(response).unwrap()
            }
        });
        match is_notification {
            true => JsonRpcNotification::new_with_result_params(result, id.to_string()).into(),
            false => JsonRpcResponse::new(result, id).into(),
        }
    }
}
