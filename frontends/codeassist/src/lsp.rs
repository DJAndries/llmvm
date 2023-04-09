use std::collections::HashMap;

use serde_json::Value;

use crate::jsonrpc::JsonRpcMessage;

pub const CONTENT_LENGTH_HEADER: &str = "Content-Length";
pub const CODE_ACTION_METHOD: &str = "textDocument/codeAction";
pub const COMMAND_METHOD: &str = "workspace/executeCommand";
pub const INITIALIZE_METHOD: &str = "initialize";
pub const EXIT_METHOD: &str = "exit";

pub const CODE_COMPLETE_COMMAND_ID: &str = "llmvm-codeassist/complete";

#[derive(Clone, Debug)]
pub struct LspMessage {
    pub headers: HashMap<String, String>,
    pub payload: JsonRpcMessage,
}
