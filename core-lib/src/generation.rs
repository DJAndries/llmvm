use std::str::FromStr;

use llmvm_protocol::service::{BackendRequest, BackendResponse};
use llmvm_protocol::tower::Service;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, GenerationParameters, GenerationRequest,
    Message, MessageRole, ModelDescription, NotificationStream, ServiceResponse,
};
use serde_json::Value;

use tracing::{debug, info};

use crate::error::CoreError;
use crate::presets::load_preset;
use crate::prompts::ReadyPrompt;
use crate::threads::get_thread_messages;
use crate::{LLMVMCore, Result};

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

    pub(super) async fn prepare_for_generate(
        &self,
        request: &GenerationRequest,
    ) -> Result<(
        BackendGenerationRequest,
        ModelDescription,
        Option<Vec<Message>>,
    )> {
        let parameters = match &request.preset_id {
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

        let model = parameters
            .model
            .ok_or(CoreError::MissingParameter("model"))?;
        let model_description =
            ModelDescription::from_str(&model).map_err(|_| CoreError::ModelDescriptionParse)?;
        let is_chat_model = model_description.is_chat_model();
        let prompt_parameters = parameters
            .prompt_parameters
            .unwrap_or(Value::Object(Default::default()));

        let prompt = match parameters.custom_prompt_template {
            Some(template) => {
                ReadyPrompt::from_custom_template(&template, &prompt_parameters, is_chat_model)?
            }
            None => match parameters.prompt_template_id {
                Some(template_id) => {
                    ReadyPrompt::from_stored_template(
                        &template_id,
                        &prompt_parameters,
                        is_chat_model,
                    )
                    .await?
                }
                None => ReadyPrompt::from_custom_prompt(
                    request
                        .custom_prompt
                        .as_ref()
                        .ok_or(CoreError::TemplateNotFound)?
                        .clone(),
                ),
            },
        };

        let mut thread_messages = match request.existing_thread_id.as_ref() {
            Some(thread_id) => Some(get_thread_messages(thread_id).await?),
            None => None,
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
                    role: MessageRole::System,
                    content,
                },
            );
        }

        let thread_messages_to_save = match request.save_thread {
            true => {
                let mut clone = thread_messages.clone().unwrap_or_default();
                clone.push(Message {
                    role: MessageRole::User,
                    content: prompt.main_prompt.clone(),
                });
                Some(clone)
            }
            false => None,
        };

        let backend_request = BackendGenerationRequest {
            model,
            prompt: prompt.main_prompt,
            max_tokens: parameters
                .max_tokens
                .ok_or(CoreError::MissingParameter("max_tokens"))?,
            thread_messages,
            model_parameters: parameters.model_parameters,
        };

        info!(
            "Sending backend request with prompt: {}",
            backend_request.prompt
        );
        debug!(
            "Thread messages for requests: {:#?}",
            backend_request.thread_messages
        );
        Ok((backend_request, model_description, thread_messages_to_save))
    }
}
