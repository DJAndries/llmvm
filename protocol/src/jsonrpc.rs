use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{ProtocolError, ProtocolErrorType};

pub const ID_KEY: &str = "id";
pub const METHOD_KEY: &str = "method";
const JSON_RPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    pub jsonrpc_version: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(rename = "jsonrpc")]
    pub jsonrpc_version: String,
    pub result: Option<Value>,
    pub error: Option<JsonRpcResponseError>,
    pub id: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    #[serde(rename = "jsonrpc")]
    pub jsonrpc_version: String,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponseError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Clone, PartialEq, Debug)]
#[repr(i32)]
pub enum JsonRpcErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
}

impl From<i32> for JsonRpcErrorCode {
    fn from(value: i32) -> Self {
        match value {
            -32700 => Self::ParseError,
            -32600 => Self::InvalidRequest,
            -32601 => Self::MethodNotFound,
            -32602 => Self::InvalidParams,
            -32603 => Self::InternalError,
            _ => Self::InternalError,
        }
    }
}

impl From<ProtocolErrorType> for JsonRpcErrorCode {
    fn from(value: ProtocolErrorType) -> Self {
        match value {
            ProtocolErrorType::BadRequest => JsonRpcErrorCode::InvalidRequest,
            ProtocolErrorType::Unauthorized => JsonRpcErrorCode::InvalidRequest,
            ProtocolErrorType::Internal => JsonRpcErrorCode::InternalError,
        }
    }
}

impl Into<ProtocolErrorType> for JsonRpcErrorCode {
    fn into(self) -> ProtocolErrorType {
        match self {
            Self::ParseError => ProtocolErrorType::BadRequest,
            Self::InvalidRequest => ProtocolErrorType::BadRequest,
            Self::MethodNotFound => ProtocolErrorType::BadRequest,
            Self::InvalidParams => ProtocolErrorType::BadRequest,
            Self::InternalError => ProtocolErrorType::Internal,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcRequest {
    pub fn new(method: String, params: Option<Value>) -> Self {
        Self {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            method,
            params,
            id: Value::Null,
        }
    }
}

impl JsonRpcResponse {
    pub fn new(result: Result<Value, ProtocolError>, id: Value) -> Self {
        let (error, result) = match result {
            Ok(result) => (None, Some(result)),
            Err(e) => (
                Some(JsonRpcResponseError {
                    code: JsonRpcErrorCode::from(e.error_type.clone()) as i32,
                    message: e.to_string(),
                    data: None,
                }),
                None,
            ),
        };
        JsonRpcResponse {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            result,
            error,
            id: id.into(),
        }
    }
}

impl JsonRpcNotification {
    pub fn new(method: String, params: Option<Value>) -> Self {
        JsonRpcNotification {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            method,
            params,
        }
    }
}

impl From<JsonRpcRequest> for JsonRpcMessage {
    fn from(value: JsonRpcRequest) -> Self {
        Self::Request(value)
    }
}

impl From<JsonRpcResponse> for JsonRpcMessage {
    fn from(value: JsonRpcResponse) -> Self {
        Self::Response(value)
    }
}

impl From<JsonRpcNotification> for JsonRpcMessage {
    fn from(value: JsonRpcNotification) -> Self {
        Self::Notification(value)
    }
}

impl TryFrom<serde_json::Value> for JsonRpcMessage {
    type Error = serde_json::Error;

    fn try_from(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(match value.get(METHOD_KEY).is_some() {
            true => match value.get(ID_KEY).is_some() {
                true => JsonRpcMessage::Request(serde_json::from_value(value)?),
                false => JsonRpcMessage::Notification(serde_json::from_value(value)?),
            },
            false => JsonRpcMessage::Response(serde_json::from_value(value)?),
        })
    }
}
