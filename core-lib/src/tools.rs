use llmvm_protocol::{ToolCall, ToolType};
use serde_json::{Map, Value};

use crate::error::CoreError;
use crate::sessions::SessionSubscriberInfo;
use crate::Result;

const TEXT_TOOL_CALL_HEADER: &str = "$$llmvm_tool$$ ";
const MULTILINE_TOKEN: &str = "$$multiline$$";

pub(super) fn inject_client_id_into_tools(subscriber_infos: &mut [SessionSubscriberInfo]) {
    for info in subscriber_infos {
        if let Some(tools) = &mut info.tools {
            for tool in tools {
                tool.name = format!("{}_{}", tool.name, info.client_id);
            }
        }
    }
}

fn init_tool_call<'a>(
    subscriber_infos: &'a [SessionSubscriberInfo],
    function_name: &str,
) -> Result<(ToolCall, &'a Value)> {
    for info in subscriber_infos {
        if function_name.ends_with(&format!("_{}", info.client_id)) {
            let original_name =
                function_name[..function_name.len() - info.client_id.len() - 1].to_string();

            let tool = info
                .tools
                .as_ref()
                .and_then(|tools| tools.iter().find(|t| t.name == function_name))
                .ok_or(CoreError::ToolCallParse)?;

            return Ok((
                ToolCall {
                    id: None,
                    name: original_name,
                    client_id: info.client_id.clone(),
                    arguments: Value::Object(Map::new()),
                },
                &tool.input_schema,
            ));
        }
    }
    Err(CoreError::ToolCallParse)
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
        .ok_or(CoreError::ToolCallParse)?;

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

    let mut current_tool_call_and_schema = None;

    let mut current_index = 0;
    let mut result = Vec::new();

    while let Some(call_header_index) = text[current_index..].find(TEXT_TOOL_CALL_HEADER) {
        let function_name_index = call_header_index + TEXT_TOOL_CALL_HEADER.len();
        let mut call_chars = text[function_name_index..].chars();

        let mut state = ExtractState::ReadFunctionName;
        let mut arg_name = String::new();
        let mut current_value = String::new();
        let mut chars_processed = 0;

        while let Some(c) = call_chars.next() {
            chars_processed += 1;
            match state {
                ExtractState::ReadFunctionName => {
                    if c == ':' {
                        current_tool_call_and_schema =
                            Some(init_tool_call(subscriber_infos, &current_value)?);
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
                            let peek = call_chars.as_str();
                            if peek.starts_with(&MULTILINE_TOKEN[1..]) {
                                // skip the token and the newline after it
                                call_chars.nth(MULTILINE_TOKEN.len() - 1);
                                ExtractState::MultilineArg
                            } else {
                                return Err(CoreError::ToolCallParse);
                            }
                        }
                        _ => return Err(CoreError::ToolCallParse),
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
                        let peek = call_chars.as_str();
                        if peek.starts_with(&MULTILINE_TOKEN[1..]) {
                            call_chars.nth(MULTILINE_TOKEN.len() - 2);

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

        current_index = function_name_index + chars_processed;
    }

    Ok(result)
}

pub(super) fn generate_text_tools_prompt(subscriber_infos: &[SessionSubscriberInfo]) -> String {
    let mut prompt = String::new();
    for info in subscriber_infos {
        if let Some(tools) = &info.tools {
            for tool in tools {
                if let ToolType::Text = tool.tool_type {
                    // Add tool name and description
                    prompt.push_str(&format!(
                        "Tool: {}\nDescription: {}\n",
                        tool.name, tool.description
                    ));

                    // Parse input schema and extract properties
                    if let Value::Object(schema) = &tool.input_schema {
                        if let Some(Value::Object(properties)) = schema.get("properties") {
                            prompt.push_str("Arguments:\n");

                            // Process each property in the schema
                            for (prop_name, prop_value) in properties {
                                if let Value::Object(prop_obj) = prop_value {
                                    let description = prop_obj
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");

                                    prompt.push_str(&format!("- {}: {}\n", prop_name, description));
                                }
                            }

                            // Add usage instruction
                            prompt.push_str("\nTo use this tool, output the following, preserving the quotes:\n");
                            prompt.push_str(&format!("$$llmvm_tool$$ {}:", tool.name));

                            let mut props = Vec::new();
                            // Add example argument placeholders
                            for prop_name in properties.keys() {
                                // Check if argument description contains "multiline"
                                let prop_desc = properties[prop_name]
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_lowercase();

                                if prop_desc.contains("multiline") {
                                    props.push(format!(
                                        "$$multiline$$\n<{} value>\n$$multiline$$",
                                        prop_name
                                    ));
                                } else {
                                    props.push(format!("{}=\"value of {}\"", prop_name, prop_name));
                                }
                            }
                            prompt.push_str(&format!("{};\n\n", props.join(" ")))
                        }
                    }
                }
            }
        }
    }

    prompt
}
