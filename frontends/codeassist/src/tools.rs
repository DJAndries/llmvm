use std::{collections::HashMap, fs::canonicalize, sync::Arc};

use anyhow::{anyhow, bail, Result};
use llmvm_protocol::{
    service::{CoreRequest, CoreResponse},
    tower::Service,
    BoxedService, StoreToolCallResultsRequest, Tool, ToolCall, ToolCallResult, ToolType,
};
use lsp_types::{
    request::ApplyWorkspaceEdit, ApplyWorkspaceEditParams, Position, Range, TextEdit, WorkspaceEdit,
};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::error;
use url::Url;

use crate::{
    lsp::LspMessage,
    service::{LspMessageInfo, LspMessageService},
};

const REPLACE_LINES_TOOL_NAME: &str = "replace_lines";
const INSERT_LINE_TOOL_NAME: &str = "insert_line";

pub const FILE_URI_ARG: &str = "file_uri";
pub const START_LINE_ARG: &str = "start_line";
pub const END_LINE_ARG: &str = "end_line";
pub const INSERT_LINE_ARG: &str = "line";
pub const CONTENT_ARG: &str = "content";

pub struct Tools {
    session_id: String,
    client_id: String,
    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
    passthrough_service: LspMessageService,
    use_native_tools: bool,
}

impl Tools {
    pub fn new(
        session_id: String,
        client_id: String,
        llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
        passthrough_service: LspMessageService,
        use_native_tools: bool,
    ) -> Self {
        Self {
            session_id,
            client_id,
            llmvm_core_service,
            passthrough_service,
            use_native_tools,
        }
    }

    pub fn get_definitions(use_native_tools: bool) -> Vec<Tool> {
        let tool_type = match use_native_tools {
            true => ToolType::Native,
            false => ToolType::Text,
        };
        vec![
            Tool {
                name: REPLACE_LINES_TOOL_NAME.to_string(),
                description: "Replace lines in a file".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        FILE_URI_ARG: {"type": "string", "description": "URI of file to edit"},
                        START_LINE_ARG: {"type": "integer", "description": "Starting line number, inclusive"},
                        END_LINE_ARG: {"type": "integer", "description": "Ending line number, inclusive"},
                        CONTENT_ARG: {"type": "string", "description": "New multiline content to replace the lines"}
                    },
                    "required": [FILE_URI_ARG, START_LINE_ARG, END_LINE_ARG, CONTENT_ARG]
                }),
                tool_type: tool_type.clone(),
            },
            Tool {
                name: INSERT_LINE_TOOL_NAME.to_string(),
                description: "Insert a new line after the specified line number in a file"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        FILE_URI_ARG: {"type": "string", "description": "URI of file to edit"},
                        INSERT_LINE_ARG: {"type": "integer", "description": "The line number to insert the content; existing content at the line number will be moved downwards"},
                        CONTENT_ARG: {"type": "string", "description": "Multiline content to insert as new line"}
                    },
                    "required": [FILE_URI_ARG, INSERT_LINE_ARG, CONTENT_ARG]
                }),
                tool_type,
            },
        ]
    }

    async fn process_tool_call(
        &mut self,
        edits: &mut HashMap<Url, Vec<TextEdit>>,
        tool_call: ToolCall,
    ) -> Result<Option<ToolCallResult>> {
        if tool_call.client_id.unwrap_or_default() != self.client_id {
            return Ok(None);
        }
        let arguments = tool_call
            .arguments
            .as_object()
            .ok_or_else(|| anyhow!("tool arguments is not in object format"))?;

        let file_uri = Url::from_file_path(canonicalize(
            arguments
                .get(FILE_URI_ARG)
                .ok_or_else(|| anyhow!("missing file uri argument"))?
                .as_str()
                .ok_or_else(|| anyhow!("failed to parse file uri argument"))?,
        )?)
        .map_err(|_| anyhow!("failed to parse file path"))?;

        let content = arguments
            .get(CONTENT_ARG)
            .ok_or_else(|| anyhow!("missing content argument"))?
            .as_str()
            .ok_or_else(|| anyhow!("failed to parse content argument"))?
            .to_string();

        let range = match tool_call.name.as_str() {
            INSERT_LINE_TOOL_NAME => {
                let line = arguments
                    .get(INSERT_LINE_ARG)
                    .ok_or_else(|| anyhow!("missing insert line argument"))?
                    .as_i64()
                    .ok_or_else(|| anyhow!("failed to parse insert line argument"))?
                    - 1;
                if line < 0 {
                    bail!("invalid line number");
                }
                let position = Position::new(line as u32, 0);
                Range::new(position.clone(), position)
            }
            REPLACE_LINES_TOOL_NAME => {
                let start_line = arguments
                    .get(START_LINE_ARG)
                    .ok_or_else(|| anyhow!("missing start line argument"))?
                    .as_i64()
                    .ok_or_else(|| anyhow!("failed to parse start line argument"))?
                    - 1;
                let end_line = arguments
                    .get(END_LINE_ARG)
                    .ok_or_else(|| anyhow!("missing start line argument"))?
                    .as_i64()
                    .ok_or_else(|| anyhow!("failed to parse start line argument"))?;
                if start_line < 0 || end_line < 0 {
                    bail!("invalid line number");
                }
                Range::new(
                    Position::new(start_line as u32, 0),
                    Position::new(end_line as u32, 0),
                )
            }
            _ => bail!("unrecognized tool name"),
        };

        edits
            .entry(file_uri)
            .or_default()
            .push(TextEdit::new(range, content));
        Ok(match self.use_native_tools {
            true => Some(ToolCallResult {
                id: tool_call.id.unwrap_or_default(),
                result: None,
                is_fatal_error: false,
            }),
            false => None,
        })
    }

    pub async fn process_tool_calls(&mut self, tool_calls: Vec<ToolCall>, session_tag: &str) {
        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        let mut tool_call_results = match self.use_native_tools {
            true => Some(Vec::new()),
            false => None,
        };

        for tool_call in tool_calls {
            match self.process_tool_call(&mut edits, tool_call).await {
                Err(e) => {
                    // TODO(djandries): notify lsp client of failure
                    error!("Failed to process tool call: {e}");
                }
                Ok(tool_call_result) => {
                    if let Some(tool_call_result) = tool_call_result {
                        tool_call_results.as_mut().unwrap().push(tool_call_result);
                    }
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        if let Err(e) = self
            .passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<ApplyWorkspaceEdit>(ApplyWorkspaceEditParams {
                    label: None,
                    edit: WorkspaceEdit::new(edits),
                })
                .unwrap(),
                false,
            ))
            .await
        {
            error!("Failed to apply workspace edit from tool call: {e}")
        }

        if let Some(results) = tool_call_results {
            if let Err(e) = self
                .llmvm_core_service
                .lock()
                .await
                .call(CoreRequest::StoreToolCallResults(
                    StoreToolCallResultsRequest {
                        session_id: self.session_id.clone(),
                        session_tag: session_tag.to_string(),
                        client_id: self.client_id.clone(),
                        results,
                    },
                ))
                .await
            {
                error!("failed to store tool call results: {e}");
            }
        }
    }
}
