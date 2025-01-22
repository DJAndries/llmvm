use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use llmvm_protocol::{
    BackendTool, BackendToolCall, GetThreadMessagesRequest, Message, MessageRole, Tool, ToolCall,
    ToolCallResult, ToolType,
};
use notify::Watcher;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::error::CoreError;
use crate::sessions::{SessionSubscriberInfo, SESSION_INFO_FILENAME};
use crate::threads::{get_thread_messages, save_thread};
use crate::Result;

const TEXT_TOOL_CALL_HEADER: &str = "$$llmvm_tool$$ ";
const MULTILINE_TOKEN: &str = "$$multiline$$";

pub(super) struct ToolCallHelper {
    tool_results: Arc<Mutex<HashMap<String, Option<ToolCallResult>>>>,
    session_path: Option<PathBuf>,
    client_id: Option<String>,
    thread_id: String,
}

impl ToolCallHelper {
    pub fn new(
        tool_call_ids: Vec<String>,
        session_path: Option<PathBuf>,
        client_id: Option<String>,
        thread_id: String,
    ) -> Self {
        let tool_results = Arc::new(Mutex::new(
            tool_call_ids
                .into_iter()
                .map(|id| (id, None))
                .collect::<HashMap<String, Option<ToolCallResult>>>(),
        ));

        Self {
            tool_results,
            session_path,
            client_id,
            thread_id,
        }
    }

    pub async fn run(self) -> Result<()> {
        let (finish_tx, mut finish_rx) = mpsc::unbounded_channel::<()>();
        let _watcher = match self.session_path {
            Some(session_path) => {
                let tool_results = self.tool_results.clone();
                // Watch session subscribers for tool call results
                let mut watcher = notify::recommended_watcher(
                    move |res: std::result::Result<notify::Event, notify::Error>| {
                        if let Ok(event) = res {
                            let event_path = event.paths.first().unwrap();
                            let filename =
                                event_path.file_name().unwrap_or_default().to_string_lossy();

                            if let notify::EventKind::Modify(_) = &event.kind {
                                if filename == SESSION_INFO_FILENAME {
                                    return;
                                }
                                if let Ok(info) =
                                    fs::read(event_path).map_err(|_| ()).and_then(|d| {
                                        serde_json::from_slice::<SessionSubscriberInfo>(&d)
                                            .map_err(|_| ())
                                    })
                                {
                                    let mut tool_results_guard = tool_results.lock().unwrap();
                                    for result in info.tool_call_results {
                                        if tool_results_guard.contains_key(&result.id) {
                                            tool_results_guard
                                                .insert(result.id.clone(), Some(result));
                                        }
                                    }
                                    if tool_results_guard.values().all(|v| v.is_some()) {
                                        let _ = finish_tx.send(());
                                    }
                                }
                            }
                        }
                    },
                )?;

                watcher.watch(&session_path, notify::RecursiveMode::NonRecursive)?;

                Some(watcher)
            }
            None => None,
        };

        // TODO(djandries): for mcp clients, use tokio select select with a timeout
        let _ = finish_rx.recv().await;
        drop(_watcher);

        let (mut thread_messages, _) = get_thread_messages(&GetThreadMessagesRequest {
            thread_id: Some(self.thread_id.clone()),
            ..Default::default()
        })
        .await?;
        thread_messages.push(Message {
            client_id: self.client_id,
            role: MessageRole::User,
            content: None,
            tool_calls: None,
            tool_call_results: Some(
                self.tool_results
                    .lock()
                    .unwrap()
                    .values()
                    .into_iter()
                    .map(|v| v.clone().unwrap())
                    .collect(),
            ),
        });
        save_thread(&self.thread_id, thread_messages).await?;
        Ok(())
    }
}

pub(super) fn inject_client_id_into_tools(subscriber_infos: &mut [SessionSubscriberInfo]) {
    for info in subscriber_infos {
        if let Some(tools) = &mut info.tools {
            for tool in tools {
                tool.name = format!("{}_{}", tool.name, info.client_id);
            }
        }
    }
}

/// Extracts the client tool definition, original tool name (without client id suffix), and client id
fn find_subscriber_tool<'a>(
    subscriber_infos: &'a [SessionSubscriberInfo],
    function_name: &str,
) -> Option<(&'a Tool, String, String)> {
    for info in subscriber_infos {
        if function_name.ends_with(&format!("_{}", info.client_id)) {
            let original_name =
                function_name[..function_name.len() - info.client_id.len() - 1].to_string();

            return info
                .tools
                .as_ref()
                .and_then(|tools| tools.iter().find(|t| t.name == function_name))
                .map(|tool| (tool, original_name, info.client_id.to_string()));
        }
    }
    None
}

/// Removes client id suffix from tool name and adds client id
pub(super) fn backend_tool_calls_to_tool_calls(
    backend_calls: Vec<BackendToolCall>,
    subscriber_infos: Option<&[SessionSubscriberInfo]>,
) -> Result<Vec<ToolCall>> {
    backend_calls
        .into_iter()
        .map(|backend_call| {
            let mut tool_call = ToolCall {
                id: Some(backend_call.id),
                name: backend_call.name,
                client_id: None,
                arguments: backend_call.arguments,
            };
            if let Some(subscriber_infos) = subscriber_infos {
                if let Some((_, original_name, client_id)) =
                    find_subscriber_tool(subscriber_infos, &tool_call.name)
                {
                    tool_call.name = original_name;
                    tool_call.client_id = Some(client_id);
                }
            }
            if tool_call.client_id.is_none() {
                return Err(CoreError::ToolCallParse("tool not found"));
            }
            Ok(tool_call)
        })
        .collect()
}

fn init_text_tool_call<'a>(
    subscriber_infos: &'a [SessionSubscriberInfo],
    function_name: &str,
) -> Result<(ToolCall, &'a Value)> {
    let (tool, original_name, client_id) =
        find_subscriber_tool(&subscriber_infos, function_name)
            .ok_or(CoreError::ToolCallParse("tool does not exist for client"))?;

    Ok((
        ToolCall {
            id: None,
            name: original_name,
            client_id: Some(client_id),
            arguments: Value::Object(Map::new()),
        },
        &tool.input_schema,
    ))
}

fn insert_tool_call_argument(
    current_tool_call_and_schema: &mut (ToolCall, &Value),
    arg_name: &str,
    current_value: &str,
) -> Result<()> {
    // Check if property is defined as integer in schema
    let is_integer = current_tool_call_and_schema
        .1
        .get("properties")
        .and_then(|props| props.get(arg_name))
        .and_then(|prop| prop.get("type"))
        .and_then(|t| t.as_str())
        .map(|t| t == "integer")
        .ok_or(CoreError::ToolCallParse(
            "failed to check type of tool property",
        ))?;

    let value = if is_integer {
        if let Ok(num) = current_value.parse::<i64>() {
            Value::Number(num.into())
        } else {
            Value::String(current_value.to_string())
        }
    } else {
        Value::String(current_value.to_string())
    };

    current_tool_call_and_schema
        .0
        .arguments
        .as_object_mut()
        .unwrap()
        .insert(arg_name.to_string(), value);
    Ok(())
}

pub(super) fn extract_text_tool_calls(
    subscriber_infos: &[SessionSubscriberInfo],
    text: &str,
) -> Result<Vec<ToolCall>> {
    #[derive(PartialEq)]
    enum ExtractState {
        ReadFunctionName,
        ExpectArgName,
        ReadArgName,
        ExpectArgValue,
        ReadArgValue,
        MultilineArg,
    }

    let mut result = Vec::new();

    let mut chars = text.chars();

    while let Some(call_header_index) = chars.as_str().find(TEXT_TOOL_CALL_HEADER) {
        chars.nth(call_header_index + TEXT_TOOL_CALL_HEADER.len() - 1);

        let mut current_tool_call_and_schema = None;

        let mut state = ExtractState::ReadFunctionName;
        let mut arg_name = String::new();
        let mut current_value = String::new();

        while let Some(c) = chars.next() {
            match state {
                ExtractState::ReadFunctionName => {
                    if c == ':' {
                        current_tool_call_and_schema =
                            Some(init_text_tool_call(subscriber_infos, &current_value)?);
                        current_value.clear();
                        state = ExtractState::ExpectArgName;
                    } else {
                        current_value.push(c);
                    }
                }
                ExtractState::ExpectArgName => {
                    if c.is_whitespace() {
                        continue;
                    } else if c.is_alphanumeric() {
                        arg_name.push(c);
                        state = ExtractState::ReadArgName;
                    } else if c == ';' {
                        break;
                    }
                }
                ExtractState::ReadArgName => {
                    if c == '=' {
                        state = ExtractState::ExpectArgValue;
                    } else {
                        arg_name.push(c);
                    }
                }
                ExtractState::ExpectArgValue => {
                    state = match c {
                        '"' => ExtractState::ReadArgValue,
                        '$' => {
                            let peek = chars.as_str();
                            if peek.starts_with(&MULTILINE_TOKEN[1..]) {
                                // skip the token and the newline after it
                                chars.nth(MULTILINE_TOKEN.len() - 1);
                                ExtractState::MultilineArg
                            } else {
                                return Err(CoreError::ToolCallParse("unrecognized '$' token"));
                            }
                        }
                        _ => {
                            return Err(CoreError::ToolCallParse(
                                "unexpected argument value start token",
                            ))
                        }
                    };
                }
                ExtractState::ReadArgValue => {
                    match c {
                        '"' => {
                            insert_tool_call_argument(
                                current_tool_call_and_schema.as_mut().unwrap(),
                                &arg_name,
                                &current_value,
                            )?;
                            current_value.clear();
                            arg_name.clear();
                            state = ExtractState::ExpectArgName;
                        }
                        _ => {
                            current_value.push(c);
                        }
                    };
                }
                ExtractState::MultilineArg => {
                    if c == '$' {
                        let peek = chars.as_str();
                        if peek.starts_with(&MULTILINE_TOKEN[1..]) {
                            chars.nth(MULTILINE_TOKEN.len() - 2);

                            insert_tool_call_argument(
                                current_tool_call_and_schema.as_mut().unwrap(),
                                &arg_name,
                                &current_value,
                            )?;
                            current_value.clear();
                            arg_name.clear();

                            state = ExtractState::ExpectArgName;
                            continue;
                        }
                    }
                    current_value.push(c);
                }
            }
        }
        result.push(current_tool_call_and_schema.take().unwrap().0);
    }

    Ok(result)
}

#[derive(Serialize)]
pub(super) struct TextToolPromptParameters {
    name: String,
    description: String,
    arguments: Vec<String>,
    usage_format: String,
}

pub(super) fn generate_native_tools_from_subscribers(
    subscriber_infos: &[SessionSubscriberInfo],
) -> Option<Vec<BackendTool>> {
    let mut result: Option<Vec<_>> = None;
    for info in subscriber_infos {
        if let Some(tools) = &info.tools {
            for tool in tools {
                if let ToolType::Native = tool.tool_type {
                    result.get_or_insert_default().push(tool.clone().into());
                }
            }
        }
    }
    result
}

pub(super) fn generate_text_tools_prompt_parameters(
    subscriber_infos: &[SessionSubscriberInfo],
) -> Vec<TextToolPromptParameters> {
    let mut result = Vec::new();
    for info in subscriber_infos {
        if let Some(tools) = &info.tools {
            for tool in tools {
                if let ToolType::Text = tool.tool_type {
                    // Parse input schema and extract properties
                    if let Value::Object(schema) = &tool.input_schema {
                        if let Some(Value::Object(properties)) = schema.get("properties") {
                            let mut arguments = Vec::new();

                            // Process each property in the schema
                            for (prop_name, prop_value) in properties {
                                if let Value::Object(prop_obj) = prop_value {
                                    let description = prop_obj
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");

                                    arguments.push(format!("{}: {}\n", prop_name, description));
                                }
                            }

                            let mut usage_format = String::new();
                            usage_format.push_str(&format!("$$llmvm_tool$$ {}:", tool.name));

                            let mut props = Vec::new();
                            // Add example argument placeholders
                            for prop_name in properties.keys() {
                                // Check if argument description contains "multiline"
                                let prop_desc = properties[prop_name]
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_lowercase();

                                if prop_desc.to_lowercase().contains("multiline") {
                                    props.push(format!(
                                        "{}=$$multiline$$\n<{} value>\n$$multiline$$",
                                        prop_name, prop_name
                                    ));
                                } else {
                                    props.push(format!("{}=\"value of {}\"", prop_name, prop_name));
                                }
                            }
                            usage_format.push_str(&format!("{};\n\n", props.join(" ")));

                            result.push(TextToolPromptParameters {
                                name: tool.name.clone(),
                                description: tool.description.clone(),
                                arguments,
                                usage_format,
                            });
                        }
                    }
                }
            }
        }
    }

    result
}
