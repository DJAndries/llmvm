use std::{
    error::Error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tower::Service;

use crate::{
    stdio::{BackendRequest, BackendResponse, CoreRequest, CoreResponse},
    Backend, Core,
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

pub type ServiceError = Box<dyn Error + Send + Sync + 'static>;
pub type ServiceFuture<Response> =
    Pin<Box<dyn Future<Output = Result<Response, ServiceError>> + Send>>;
pub type BoxedService<Request, Response> = Box<
    dyn Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Sync,
>;

impl<B> Service<BackendRequest> for BackendService<B>
where
    B: Backend + 'static,
{
    type Response = BackendResponse;
    type Error = ServiceError;
    type Future = ServiceFuture<BackendResponse>;

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
                    .map(|v| BackendResponse::Generation(v)),
            }?)
        })
    }
}

impl<C> Service<CoreRequest> for CoreService<C>
where
    C: Core + 'static,
{
    type Response = CoreResponse;
    type Error = ServiceError;
    type Future = ServiceFuture<CoreResponse>;

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
                    .map(|v| CoreResponse::Generation(v)),
                CoreRequest::InitProject => core.init_project().map(|_| CoreResponse::InitProject),
            }?)
        })
    }
}
