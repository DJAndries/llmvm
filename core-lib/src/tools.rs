use llmvm_protocol::{ToolCall, ToolType};
use serde::Serialize;
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
                .ok_or(CoreError::ToolCallParse("tool does not exist for client"))?;

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
    Err(CoreError::ToolCallParse("tool does not exist"))
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
