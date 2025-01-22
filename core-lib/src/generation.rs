use std::str::FromStr;

use llmvm_protocol::service::{BackendRequest, BackendResponse};
use llmvm_protocol::tower::Service;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, BackendMessage, BackendToolCall,
    GenerationParameters, GenerationRequest, GenerationResponse, GetThreadMessagesRequest, Message,
    MessageRole, ModelDescription, NotificationStream, ServiceResponse,
};
use serde_json::Value;

use tracing::{debug, error, info};

use crate::error::CoreError;
use crate::presets::load_preset;
use crate::prompts::ReadyPrompt;
use crate::sessions::{
    get_session_prompt_parameters, get_session_subscribers, session_path, SessionSubscriberInfo,
};
use crate::threads::{get_thread_messages, maybe_save_thread_messages_and_get_thread_id};
use crate::tools::{
    backend_tool_calls_to_tool_calls, extract_text_tool_calls,
    generate_native_tools_from_subscribers, generate_text_tools_prompt_parameters,
    inject_client_id_into_tools, ToolCallHelper,
};
use crate::{LLMVMCore, Result};

const TEXT_TOOLS_PARAM_NAME: &str = "text_tools";

fn merge_generation_parameters(
    preset_parameters: GenerationParameters,
    mut request_parameters: GenerationParameters,
) -> GenerationParameters {
    GenerationParameters {
        model: request_parameters.model.or(preset_parameters.model),
        prompt_template_id: request_parameters
            .prompt_template_id
            .or(preset_parameters.prompt_template_id),
        custom_prompt_template: request_parameters
            .custom_prompt_template
            .or(preset_parameters.custom_prompt_template),
        max_tokens: request_parameters
            .max_tokens
            .or(preset_parameters.max_tokens),
        model_parameters: preset_parameters
            .model_parameters
            .map(|mut parameters| {
                parameters.extend(
                    request_parameters
                        .model_parameters
                        .take()
                        .unwrap_or_default(),
                );
                parameters
            })
            .or(request_parameters.model_parameters),
        prompt_parameters: request_parameters
            .prompt_parameters
            .or(preset_parameters.prompt_parameters),
    }
}

pub(super) struct GenerationPreparation {
    pub model_description: ModelDescription,
    pub thread_messages_to_save: Option<Vec<Message>>,
    pub existing_thread_id: Option<String>,
    pub subscriber_infos: Option<Vec<SessionSubscriberInfo>>,
    pub text_tools_used: bool,
}

impl LLMVMCore {
    pub(super) async fn send_generate_request(
        &self,
        request: BackendGenerationRequest,
        model_description: &ModelDescription,
    ) -> Result<BackendGenerationResponse> {
        let mut clients_guard = self.clients.lock().await;
        let client = self
            .get_client(&mut clients_guard, model_description)
            .await?;
        let resp_future = client.call(BackendRequest::Generation(request));
        drop(clients_guard);
        let resp = resp_future
            .await
            .map_err(|e| CoreError::Protocol(e.into()))?;
        match resp {
            ServiceResponse::Single(response) => match response {
                BackendResponse::Generation(response) => Ok(response),
                _ => Err(CoreError::UnexpectedServiceResponse),
            },
            _ => Err(CoreError::UnexpectedServiceResponse),
        }
    }

    pub(super) async fn send_generate_request_for_stream(
        &self,
        request: BackendGenerationRequest,
        model_description: &ModelDescription,
    ) -> Result<NotificationStream<BackendResponse>> {
        let mut clients_guard = self.clients.lock().await;
        let client = self
            .get_client(&mut clients_guard, model_description)
            .await?;
        let resp_future = client.call(BackendRequest::GenerationStream(request));
        drop(clients_guard);
        let resp = resp_future
            .await
            .map_err(|e| CoreError::Protocol(e.into()))?;
        match resp {
            ServiceResponse::Multiple(stream) => Ok(stream),
            _ => Err(CoreError::UnexpectedServiceResponse),
        }
    }
}

pub(super) async fn prepare_for_generate(
    request: &GenerationRequest,
) -> Result<(BackendGenerationRequest, GenerationPreparation)> {
    let mut parameters = match &request.preset_id {
        Some(preset_id) => {
            let mut parameters = load_preset(&preset_id).await?;
            if let Some(request_parameters) = request.parameters.clone() {
                parameters = merge_generation_parameters(parameters, request_parameters);
            }
            parameters
        }
        None => request
            .parameters
            .clone()
            .ok_or(CoreError::MissingParameters)?,
    };
    debug!("generation parameters: {:?}", parameters);

    if parameters.max_tokens.is_none() {
        parameters.max_tokens = Some(2048);
    }

    let model = parameters
        .model
        .ok_or(CoreError::MissingParameter("model"))?;
    let model_description =
        ModelDescription::from_str(&model).map_err(|_| CoreError::ModelDescriptionParse)?;
    let is_chat_model = model_description.is_chat_model();
    let mut prompt_parameters = parameters
        .prompt_parameters
        .unwrap_or(Value::Object(Default::default()));

    let mut text_tools_used = false;
    // If using session, add frontend tools and prompt parameters
    let (subscriber_infos, native_tools) = match (&request.session_id, &request.session_tag) {
        (Some(session_id), Some(session_tag)) => {
            let mut subscribers = get_session_subscribers(&session_id, &session_tag).await?;
            inject_client_id_into_tools(&mut subscribers);

            let tools_prompt_params = generate_text_tools_prompt_parameters(&subscribers);
            if !tools_prompt_params.is_empty() {
                text_tools_used = true;
                prompt_parameters[TEXT_TOOLS_PARAM_NAME] =
                    serde_json::to_value(tools_prompt_params)?;
            }

            let session_prompt_parameters = get_session_prompt_parameters(&subscribers).await?;
            if !session_prompt_parameters.is_empty() {
                prompt_parameters
                    .as_object_mut()
                    .unwrap()
                    .extend(session_prompt_parameters);
            }

            let native_tools = generate_native_tools_from_subscribers(&subscribers);

            (Some(subscribers), native_tools)
        }
        _ => (None, None),
    };

    let mut prompt = match parameters.custom_prompt_template {
        Some(template) => {
            ReadyPrompt::from_custom_template(&template, &prompt_parameters, is_chat_model)?
        }
        None => match parameters.prompt_template_id {
            Some(template_id) => {
                ReadyPrompt::from_stored_template(&template_id, &prompt_parameters, is_chat_model)
                    .await?
            }
            None => Default::default(),
        },
    };

    if let Some(custom_prompt) = request.custom_prompt.clone() {
        prompt.main_prompt = Some(custom_prompt);
    }

    if prompt.main_prompt.is_none() {
        return Err(CoreError::TemplateNotFound);
    }

    let (mut thread_messages, existing_thread_id) =
        match request.existing_thread_id.is_some() || request.session_id.is_some() {
            true => {
                let (msgs, thread_id) = get_thread_messages(&GetThreadMessagesRequest {
                    thread_id: request.existing_thread_id.clone(),
                    session_id: request.session_id.clone(),
                    session_tag: request.session_tag.clone(),
                })
                .await?;
                (Some(msgs), Some(thread_id))
            }
            false => (None, None),
        };
    if let Some(content) = prompt.system_prompt {
        let messages = thread_messages.get_or_insert_with(|| Vec::with_capacity(1));
        messages.retain(|message| {
            if let MessageRole::System = message.role {
                false
            } else {
                true
            }
        });
        messages.insert(
            0,
            Message {
                client_id: request.client_id.clone(),
                role: MessageRole::System,
                content: Some(content),
                tool_calls: None,
                tool_call_results: None,
            },
        );
    }

    let mut tool_call_results = None;
    let mut main_prompt = Some(prompt.main_prompt.unwrap());

    if let Some(thread_messages) = thread_messages.as_mut() {
        if let Some(last_msg) = thread_messages.last() {
            if last_msg.tool_call_results.is_some() {
                tool_call_results = thread_messages.pop().unwrap().tool_call_results;

                // We want to omit any user text prompt, unless it's an explicit custom prompt
                // i.e. a message from the chat app
                if request.custom_prompt.is_none() {
                    main_prompt = None;
                }
            }
        }
    }

    let thread_messages_to_save = match request.save_thread {
        true => {
            let mut clone = thread_messages.clone().unwrap_or_default();
            clone.push(Message {
                client_id: request.client_id.clone(),
                role: MessageRole::User,
                content: main_prompt.clone(),
                tool_calls: None,
                tool_call_results: tool_call_results.clone(),
            });
            Some(clone)
        }
        false => None,
    };

    let backend_thread_messages =
        thread_messages.map(|v| v.into_iter().map(BackendMessage::from).collect());

    let backend_request = BackendGenerationRequest {
        model,
        prompt: main_prompt,
        max_tokens: parameters
            .max_tokens
            .ok_or(CoreError::MissingParameter("max_tokens"))?,
        thread_messages: backend_thread_messages,
        model_parameters: parameters.model_parameters,
        tools: native_tools,
        tool_call_results,
    };

    info!(
        "Sending backend request with prompt: {:?}",
        backend_request.prompt
    );
    debug!(
        "Thread messages for requests: {:#?}",
        backend_request.thread_messages
    );

    Ok((
        backend_request,
        GenerationPreparation {
            model_description,
            thread_messages_to_save,
            existing_thread_id,
            subscriber_infos,
            text_tools_used,
        },
    ))
}

pub(super) async fn finish_generation(
    request: &GenerationRequest,
    full_response: String,
    native_tool_calls: Option<Vec<BackendToolCall>>,
    preparation: GenerationPreparation,
    streamed: bool,
) -> Result<GenerationResponse> {
    let mut tool_calls = None;
    let mut native_tool_call_ids: Option<Vec<_>> = None;

    if let Some(subscriber_infos) = &preparation.subscriber_infos {
        if preparation.text_tools_used {
            debug!("extracting text tool calls");
            tool_calls = Some(extract_text_tool_calls(&subscriber_infos, &full_response)?);
        }
    }
    if let Some(native_tool_calls) = native_tool_calls {
        native_tool_call_ids = Some(native_tool_calls.iter().map(|c| c.id.clone()).collect());
        tool_calls
            .get_or_insert_default()
            .extend(backend_tool_calls_to_tool_calls(
                native_tool_calls,
                preparation.subscriber_infos.as_ref().map(|i| i.as_slice()),
            )?);
    }

    debug!("saving thread messages");
    let thread_id = maybe_save_thread_messages_and_get_thread_id(
        &request,
        full_response.clone(),
        tool_calls.clone(),
        preparation.thread_messages_to_save,
        preparation.existing_thread_id,
    )
    .await?;

    if let Some(tool_call_ids) = native_tool_call_ids {
        let session_path = match (&request.session_id, &request.session_tag) {
            (Some(session_id), Some(session_tag)) => Some(session_path(&session_id, &session_tag)?),
            _ => None,
        };

        if let Some(thread_id) = &thread_id {
            let client_id = request.client_id.clone();
            let thread_id = thread_id.clone();
            tokio::spawn(async move {
                let helper = ToolCallHelper::new(tool_call_ids, session_path, client_id, thread_id);
                if let Err(e) = helper.run().await {
                    error!("tool call helper task failed: {e}");
                }
            });
        }
    }

    Ok(GenerationResponse {
        response: match streamed {
            false => Some(full_response),
            true => None,
        },
        tool_call_part: None,
        thread_id,
        tool_calls,
    })
}
