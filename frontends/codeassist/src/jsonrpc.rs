use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_repr::{Deserialize_repr, Serialize_repr};

pub const ID_KEY: &str = "id";
pub const METHOD_KEY: &str = "method";

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
    pub code: JsonRpcErrorCode,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Clone, Serialize_repr, Deserialize_repr, PartialEq, Debug)]
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
