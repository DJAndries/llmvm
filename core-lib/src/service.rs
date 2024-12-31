use std::fs::create_dir;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    error::ProtocolErrorType, service::BackendResponse, Core, GenerationRequest,
    GenerationResponse, GetThreadMessagesRequest, Message, NewThreadInSessionRequest,
    NotificationStream, ProtocolError, StoreSessionPromptParameterRequest,
    SubscribeToThreadRequest, ThreadEvent, ThreadInfo,
};
use llmvm_util::{get_file_path, DirType};
use tracing::debug;

use crate::{
    error::CoreError,
    generation::{finish_generation, prepare_for_generate},
    sessions::{
        get_session_subscribers, start_new_thread_in_session, store_session_prompt_parameter,
    },
    threads::{get_thread_infos, get_thread_messages, subscribe_to_thread},
    LLMVMCore, PROJECT_DIR_NAME,
};

#[llmvm_protocol::async_trait]
impl Core for LLMVMCore {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<GenerationResponse, ProtocolError> {
        async {
            let (backend_request, preparation) = prepare_for_generate(&request).await?;

            let response = self
                .send_generate_request(backend_request, &preparation.model_description)
                .await?;

            debug!("Response: {}", response.response);

            finish_generation(&request, response.response, preparation, false).await
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn generate_stream(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<NotificationStream<GenerationResponse>, ProtocolError> {
        async {
            let (backend_request, preparation) = prepare_for_generate(&request).await?;

            let mut stream = self
                .send_generate_request_for_stream(backend_request, &preparation.model_description)
                .await?;

            Ok(stream! {
                let mut full_response = String::new();
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(response) => match response {
                            BackendResponse::GenerationStream(response) => {
                                full_response.push_str(&response.response);
                                yield Ok(GenerationResponse {
                                    response: response.response,
                                    thread_id: None,
                                    tool_calls: Default::default(),
                                });
                            }
                            _ => yield Err(CoreError::UnexpectedServiceResponse.into())
                        },
                        Err(e) => {
                            yield Err(e);
                        }
                    }
                }
                yield finish_generation(&request, full_response, preparation, true).await.map_err(|e| e.into());
            }
            .boxed())
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn get_last_thread_info(&self) -> std::result::Result<Option<ThreadInfo>, ProtocolError> {
        async { Ok(get_thread_infos().await?.drain(0..1).next()) }
            .await
            .map_err(|e: CoreError| e.into())
    }

    async fn get_all_thread_infos(&self) -> std::result::Result<Vec<ThreadInfo>, ProtocolError> {
        get_thread_infos().await.map_err(|e| e.into())
    }

    async fn get_thread_messages(
        &self,
        request: GetThreadMessagesRequest,
    ) -> std::result::Result<Vec<Message>, ProtocolError> {
        get_thread_messages(&request)
            .await
            .map_err(|e| e.into())
            .map(|(msgs, _)| msgs)
    }

    fn init_project(&self) -> std::result::Result<(), ProtocolError> {
        create_dir(PROJECT_DIR_NAME).map_err(|error| ProtocolError {
            error_type: ProtocolErrorType::Internal,
            error: Box::new(error),
        })?;
        // Call the following util method for all dir types
        // to trigger creation of project subdirectories
        get_file_path(DirType::Prompts, "", true);
        get_file_path(DirType::Presets, "", true);
        get_file_path(DirType::Threads, "", true);
        get_file_path(DirType::Logs, "", true);
        get_file_path(DirType::Config, "", true);
        get_file_path(DirType::Weights, "", true);
        Ok(())
    }

    async fn subscribe_to_thread(
        &self,
        request: SubscribeToThreadRequest,
    ) -> Result<NotificationStream<ThreadEvent>, ProtocolError> {
        async {
            Ok(stream! {
                let (mut rx, mut watcher) = subscribe_to_thread(request.clone()).await?;
                let current_subscribers = match (&request.session_id, &request.session_tag) {
                    (Some(session_id), Some(session_tag)) => {
                        Some(get_session_subscribers(session_id, session_tag).await?.into_iter().map(|i| i.client_id).collect())
                    },
                    _ => None,
                };
                yield Ok(ThreadEvent::Start { current_subscribers });
                while let Some(message) = rx.recv().await {
                    if let ThreadEvent::NewThread { .. } = &message {
                        (rx, watcher) = subscribe_to_thread(request.clone()).await?;
                    }
                    yield Ok(message);
                }
                drop(watcher);
            }
            .boxed())
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn new_thread_in_session(
        &self,
        request: NewThreadInSessionRequest,
    ) -> Result<String, ProtocolError> {
        start_new_thread_in_session(&request.session_id, &request.tag)
            .await
            .map_err(|e| e.into())
    }

    async fn store_session_prompt_parameter(
        &self,
        request: StoreSessionPromptParameterRequest,
    ) -> Result<(), ProtocolError> {
        store_session_prompt_parameter(
            &request.session_id,
            &request.session_tag,
            request.key,
            request.parameter,
        )
        .await
        .map_err(|e| e.into())
    }
}
