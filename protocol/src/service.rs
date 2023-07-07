use serde::{Deserialize, Serialize};

use std::{
    error::Error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::stream::StreamExt;
use multilink::{tower::Service, ServiceResponse};

use crate::{
    Backend, BackendGenerationRequest, BackendGenerationResponse, Core, GenerationRequest,
    GenerationResponse, Message, ThreadInfo,
};

#[derive(Clone, Serialize, Deserialize)]
pub enum BackendRequest {
    Generation(BackendGenerationRequest),
    GenerationStream(BackendGenerationRequest),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum BackendResponse {
    Generation(BackendGenerationResponse),
    GenerationStream(BackendGenerationResponse),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum CoreRequest {
    Generation(GenerationRequest),
    GenerationStream(GenerationRequest),
    GetLastThreadInfo,
    GetAllThreadInfos,
    GetThreadMessages { id: String },
    InitProject,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum CoreResponse {
    Generation(GenerationResponse),
    GenerationStream(GenerationResponse),
    GetLastThreadInfo(Option<ThreadInfo>),
    GetAllThreadInfos(Vec<ThreadInfo>),
    GetThreadMessages(Vec<Message>),
    InitProject,
}

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

pub type ServiceError = Box<dyn Error + Send + Sync + 'static>;
pub type ServiceFuture<Response> =
    Pin<Box<dyn Future<Output = Result<Response, ServiceError>> + Send>>;
pub type BoxedService<Request, Response> = Box<
    dyn Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Sync,
>;

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
                CoreRequest::GetThreadMessages { id } => core
                    .get_thread_messages(id)
                    .await
                    .map(|m| ServiceResponse::Single(CoreResponse::GetThreadMessages(m))),
                CoreRequest::InitProject => core
                    .init_project()
                    .map(|_| ServiceResponse::Single(CoreResponse::InitProject)),
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

    pub const LLMVM_CORE_CLI_COMMAND: &str = "llmvm-core-cli";
    pub const LLMVM_CORE_CLI_ARGS: [&'static str; 2] = ["--log-to-file", "stdio-server"];

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
