use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    pub fn new_request(method: String, params: Option<Value>) -> Self {
        Self::Request(JsonRpcRequest {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            method,
            params,
            id: Value::Null,
        })
    }

    pub fn new_notification(method: String, params: Option<Value>) -> Self {
        Self::Notification(JsonRpcNotification {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            method,
            params,
        })
    }

    pub fn new_response(result: Result<Value>, id: Value) -> Self {
        let (error, result) = match result {
            Ok(result) => (None, Some(result)),
            Err(e) => (
                Some(JsonRpcResponseError {
                    code: JsonRpcErrorCode::InternalError as i32,
                    message: e.to_string(),
                    data: None,
                }),
                None,
            ),
        };
        Self::Response(JsonRpcResponse {
            jsonrpc_version: JSON_RPC_VERSION.to_string(),
            result,
            error,
            id: id.into(),
        })
    }
}

impl TryFrom<serde_json::Value> for JsonRpcMessage {
    type Error = serde_json::Error;

    fn try_from(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(match value.get(ID_KEY).is_some() {
            true => match value.get(METHOD_KEY).is_some() {
                true => JsonRpcMessage::Request(serde_json::from_value(value)?),
                false => JsonRpcMessage::Response(serde_json::from_value(value)?),
            },
            false => JsonRpcMessage::Notification(serde_json::from_value(value)?),
        })
    }
}
