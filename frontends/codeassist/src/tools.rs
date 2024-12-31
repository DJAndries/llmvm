use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use llmvm_protocol::{tower::Service, Tool, ToolCall, ToolType};
use lsp_types::{
    request::ApplyWorkspaceEdit, ApplyWorkspaceEditParams, Position, Range, TextEdit, WorkspaceEdit,
};
use serde_json::json;
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
    client_id: String,
    passthrough_service: LspMessageService,
}

impl Tools {
    pub fn new(client_id: String, passthrough_service: LspMessageService) -> Self {
        Self {
            client_id,
            passthrough_service,
        }
    }

    pub fn get_definitions() -> Vec<Tool> {
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
                        CONTENT_ARG: {"type": "string", "description": "New content to replace the lines"}
                    },
                    "required": [FILE_URI_ARG, START_LINE_ARG, END_LINE_ARG, CONTENT_ARG]
                }),
                tool_type: ToolType::Text,
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
                        CONTENT_ARG: {"type": "string", "description": "Content to insert as new line"}
                    },
                    "required": [FILE_URI_ARG, INSERT_LINE_ARG, CONTENT_ARG]
                }),
                tool_type: ToolType::Text,
            },
        ]
    }

    async fn process_tool_call(&mut self, tool_call: ToolCall) -> Result<()> {
        if tool_call.client_id != self.client_id {
            return Ok(());
        }
        let arguments = tool_call
            .arguments
            .as_object()
            .ok_or_else(|| anyhow!("tool arguments is not in object format"))?;

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        let file_uri = Url::parse(
            arguments
                .get(FILE_URI_ARG)
                .ok_or_else(|| anyhow!("missing file uri argument"))?
                .as_str()
                .ok_or_else(|| anyhow!("failed to parse file uri argument"))?,
        )?;
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

        // Forward the tool call to the passthrough service
        self.passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<ApplyWorkspaceEdit>(ApplyWorkspaceEditParams {
                    label: None,
                    edit: WorkspaceEdit::new(edits),
                })
                .unwrap(),
                false,
            ))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to apply tool call change: {}", e))?;
        Ok(())
    }

    pub async fn process_tool_calls(&mut self, tool_calls: Vec<ToolCall>) {
        for tool_call in tool_calls {
            if let Err(e) = self.process_tool_call(tool_call).await {
                // TODO(djandries): notify lsp client of failure
                error!("Failed to process tool call: {e}");
            }
        }
    }
}
