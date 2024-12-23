use serde::{Deserialize, Serialize};

use std::{
    sync::Arc,
    task::{Context, Poll},
};

use futures::stream::StreamExt;
use multilink::{tower::Service, ServiceResponse};

pub use multilink::{BoxedService, ServiceError, ServiceFuture};

use crate::{
    Backend, BackendGenerationRequest, BackendGenerationResponse, Core, GenerationRequest,
    GenerationResponse, GetThreadMessagesRequest, Message, NewThreadInSessionRequest,
    SubscribeToThreadRequest, ThreadEvent, ThreadInfo,
};

/// Enum containing all types of backend requests.
#[derive(Clone, Serialize, Deserialize)]
pub enum BackendRequest {
    Generation(BackendGenerationRequest),
    GenerationStream(BackendGenerationRequest),
}

/// Enum containing all types of backend responses.
#[derive(Clone, Serialize, Deserialize)]
pub enum BackendResponse {
    Generation(BackendGenerationResponse),
    GenerationStream(BackendGenerationResponse),
}

/// Enum containing all types of core requests.
#[derive(Clone, Serialize, Deserialize)]
pub enum CoreRequest {
    Generation(GenerationRequest),
    GenerationStream(GenerationRequest),
    GetLastThreadInfo,
    GetAllThreadInfos,
    GetThreadMessages(GetThreadMessagesRequest),
    InitProject,
    SubscribeToThread(SubscribeToThreadRequest),
    NewThreadInSession(NewThreadInSessionRequest),
}

/// Enum containing all types of core responses.
#[derive(Clone, Serialize, Deserialize)]
pub enum CoreResponse {
    Generation(GenerationResponse),
    GenerationStream(GenerationResponse),
    GetLastThreadInfo(Option<ThreadInfo>),
    GetAllThreadInfos(Vec<ThreadInfo>),
    GetThreadMessages(Vec<Message>),
    InitProject,
    ListenOnThread(ThreadEvent),
    NewThreadInSession(ThreadInfo),
}

/// Service that receives [`BackendRequest`] values,
/// calls a [`Backend`] and responds with [`BackendResponse`].
pub struct BackendService<B>
where
    B: Backend,
{
    backend: Arc<B>,
}

impl<B> Clone for BackendService<B>
where
    B: Backend,
{
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
        }
    }
}

impl<B> BackendService<B>
where
    B: Backend,
{
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }
}

/// Service that receives [`CoreRequest`] values,
/// calls a [`Core`] and responds with [`CoreResponse`].
pub struct CoreService<C>
where
    C: Core,
{
    core: Arc<C>,
}

impl<C> Clone for CoreService<C>
where
    C: Core,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
        }
    }
}

impl<C> CoreService<C>
where
    C: Core,
{
    pub fn new(core: Arc<C>) -> Self {
        Self { core }
    }
}

impl<B> Service<BackendRequest> for BackendService<B>
where
    B: Backend + 'static,
{
    type Response = ServiceResponse<BackendResponse>;
    type Error = ServiceError;
    type Future = ServiceFuture<ServiceResponse<BackendResponse>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BackendRequest) -> Self::Future {
        let backend = self.backend.clone();
        Box::pin(async move {
            Ok(match req {
                BackendRequest::Generation(req) => backend
                    .generate(req)
                    .await
                    .map(|v| ServiceResponse::Single(BackendResponse::Generation(v))),
                BackendRequest::GenerationStream(req) => {
                    backend.generate_stream(req).await.map(|s| {
                        ServiceResponse::Multiple(
                            s.map(|resp| resp.map(|resp| BackendResponse::GenerationStream(resp)))
                                .boxed(),
                        )
                    })
                }
            }?)
        })
    }
}

impl<C> Service<CoreRequest> for CoreService<C>
where
    C: Core + 'static,
{
    type Response = ServiceResponse<CoreResponse>;
    type Error = ServiceError;
    type Future = ServiceFuture<ServiceResponse<CoreResponse>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: CoreRequest) -> Self::Future {
        let core = self.core.clone();
        Box::pin(async move {
            Ok(match req {
                CoreRequest::Generation(req) => core
                    .generate(req)
                    .await
                    .map(|v| ServiceResponse::Single(CoreResponse::Generation(v))),
                CoreRequest::GenerationStream(req) => core.generate_stream(req).await.map(|s| {
                    ServiceResponse::Multiple(
                        s.map(|resp| resp.map(|resp| CoreResponse::GenerationStream(resp)))
                            .boxed(),
                    )
                }),
                CoreRequest::GetLastThreadInfo => core
                    .get_last_thread_info()
                    .await
                    .map(|i| ServiceResponse::Single(CoreResponse::GetLastThreadInfo(i))),
                CoreRequest::GetAllThreadInfos => core
                    .get_all_thread_infos()
                    .await
                    .map(|i| ServiceResponse::Single(CoreResponse::GetAllThreadInfos(i))),
                CoreRequest::GetThreadMessages(req) => core
                    .get_thread_messages(req)
                    .await
                    .map(|m| ServiceResponse::Single(CoreResponse::GetThreadMessages(m))),
                CoreRequest::InitProject => core
                    .init_project()
                    .map(|_| ServiceResponse::Single(CoreResponse::InitProject)),
                CoreRequest::SubscribeToThread(req) => {
                    core.subscribe_to_thread(req).await.map(|s| {
                        ServiceResponse::Multiple(
                            s.map(|resp| resp.map(|resp| CoreResponse::ListenOnThread(resp)))
                                .boxed(),
                        )
                    })
                }
                CoreRequest::NewThreadInSession(req) => {
                    core.new_thread_in_session(req).await.map(|id| {
                        ServiceResponse::Single(CoreResponse::NewThreadInSession(ThreadInfo {
                            id,
                            modified: None,
                        }))
                    })
                }
            }?)
        })
    }
}

#[cfg(all(feature = "http-client", feature = "stdio-client"))]
pub mod util {
    use multilink::{
        http::client::HttpClientConfig, stdio::client::StdioClientConfig,
        util::service::build_service_from_config, BoxedService, ServiceError,
    };

    use super::{CoreRequest, CoreResponse};

    /// The default name of the core cli binary.
    pub const LLMVM_CORE_CLI_COMMAND: &str = "llmvm-core";
    /// CLI arguments for the core, when invoking the process for stdio communication.
    pub const LLMVM_CORE_CLI_ARGS: [&'static str; 2] = ["--log-to-file", "stdio-server"];

    /// Create a core service client that communicates with stdio or HTTP.
    /// If `http_client_config` is provided, an HTTP client will be created.
    /// Otherwise, a stdio client is created. Useful for frontends.
    pub async fn build_core_service_from_config(
        stdio_client_config: Option<StdioClientConfig>,
        http_client_config: Option<HttpClientConfig>,
    ) -> Result<BoxedService<CoreRequest, CoreResponse>, ServiceError> {
        build_service_from_config::<CoreRequest, CoreResponse>(
            LLMVM_CORE_CLI_COMMAND,
            &LLMVM_CORE_CLI_ARGS,
            stdio_client_config,
            http_client_config,
        )
        .await
    }
}
