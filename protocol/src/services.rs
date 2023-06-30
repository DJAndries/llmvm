use std::{
    error::Error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::stream::StreamExt;
use tower::{timeout::Timeout, Service};

use crate::{
    stdio::{BackendRequest, BackendResponse, CoreRequest, CoreResponse},
    Backend, Core, NotificationStream, COMMAND_TIMEOUT_SECS,
};

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

pub enum ServiceResponse<Response> {
    Single(Response),
    Multiple(NotificationStream<Response>),
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

pub fn service_with_timeout<Request, Response>(
    service: BoxedService<Request, Response>,
) -> Timeout<BoxedService<Request, Response>> {
    Timeout::new(service, Duration::from_secs(COMMAND_TIMEOUT_SECS))
}

#[cfg(all(feature = "http-client", feature = "stdio"))]
pub mod util {
    use serde::de::DeserializeOwned;

    use crate::{
        http::{HttpClient, RequestHttpConvert, ResponseHttpConvert},
        stdio::{RequestJsonRpcConvert, ResponseJsonRpcConvert, StdioClient},
        HttpClientConfig,
    };

    use super::{BoxedService, ServiceError};

    pub const LLMVM_CORE_CLI_COMMAND: &str = "llmvm-core-cli";
    pub const LLMVM_CORE_CLI_ARGS: [&'static str; 2] = ["--log-to-file", "stdio-server"];

    pub async fn build_service_from_config<Request, Response>(
        command_name: &str,
        command_arguments: &[&str],
        bin_path: Option<&str>,
        http_client_config: Option<HttpClientConfig>,
    ) -> Result<BoxedService<Request, Response>, ServiceError>
    where
        Request: RequestHttpConvert<Request>
            + RequestJsonRpcConvert<Request>
            + Clone
            + Send
            + Sync
            + 'static,
        Response: ResponseHttpConvert<Request, Response>
            + ResponseJsonRpcConvert<Request, Response>
            + Send
            + Sync
            + 'static,
    {
        Ok(match http_client_config {
            Some(config) => Box::new(HttpClient::new(config)?),
            None => Box::new(StdioClient::new(bin_path, command_name, command_arguments).await?),
        })
    }

    pub async fn build_core_service_from_config<Request, Response>(
        bin_path: Option<&str>,
        http_client_config: Option<HttpClientConfig>,
    ) -> Result<BoxedService<Request, Response>, ServiceError>
    where
        Request: RequestHttpConvert<Request>
            + RequestJsonRpcConvert<Request>
            + Clone
            + Send
            + Sync
            + 'static,
        Response: ResponseHttpConvert<Request, Response>
            + ResponseJsonRpcConvert<Request, Response>
            + Send
            + Sync
            + 'static,
    {
        build_service_from_config(
            LLMVM_CORE_CLI_COMMAND,
            &LLMVM_CORE_CLI_ARGS,
            bin_path,
            http_client_config,
        )
        .await
    }
}
