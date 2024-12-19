use std::fs::create_dir;

use async_stream::stream;
use futures::StreamExt;
use llmvm_protocol::{
    error::ProtocolErrorType, service::BackendResponse, Core, GenerationRequest,
    GenerationResponse, Message, NotificationStream, ProtocolError, ThreadInfo,
};
use llmvm_util::{get_file_path, DirType};
use tracing::debug;

use crate::{
    error::CoreError,
    threads::{
        get_thread_infos, get_thread_messages, listen_on_thread,
        maybe_save_thread_messages_and_get_thread_id,
    },
    LLMVMCore, PROJECT_DIR_NAME,
};

#[llmvm_protocol::async_trait]
impl Core for LLMVMCore {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<GenerationResponse, ProtocolError> {
        async {
            let (backend_request, model_description, thread_messages_to_save) =
                self.prepare_for_generate(&request).await?;

            let response = self
                .send_generate_request(backend_request, &model_description)
                .await?;

            debug!("Response: {}", response.response);

            let thread_id = maybe_save_thread_messages_and_get_thread_id(
                &request,
                response.response.clone(),
                thread_messages_to_save,
            )
            .await?;

            Ok(GenerationResponse {
                response: response.response,
                thread_id,
            })
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn generate_stream(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<NotificationStream<GenerationResponse>, ProtocolError> {
        async {
            let (backend_request, model_description, thread_messages_to_save) =
                self.prepare_for_generate(&request).await?;

            let mut stream = self
                .send_generate_request_for_stream(backend_request, &model_description)
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
                                });
                            }
                            _ => yield Err(CoreError::UnexpectedServiceResponse.into())
                        },
                        Err(e) => {
                            yield Err(e);
                        }
                    }
                }
                if let Ok(thread_id) = maybe_save_thread_messages_and_get_thread_id(&request, full_response, thread_messages_to_save).await {
                    yield Ok(GenerationResponse { response: String::new(), thread_id });
                }
            }.boxed())
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
        id: String,
    ) -> std::result::Result<Vec<Message>, ProtocolError> {
        get_thread_messages(&id).await.map_err(|e| e.into())
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

    async fn listen_on_thread(
        &self,
        thread_id: String,
        client_id: String,
    ) -> Result<NotificationStream<Option<Message>>, ProtocolError> {
        async {
            let (mut rx, watcher) = listen_on_thread(thread_id, client_id.clone()).await?;
            Ok(stream! {
                // Send dummy None value so that the client can immediately access and monitor the notification stream
                yield Ok(None);
                while let Some(message) = rx.recv().await {
                    yield Ok(Some(message));
                }
                drop(watcher);
            }
            .boxed())
        }
        .await
        .map_err(|e: CoreError| e.into())
    }
}
