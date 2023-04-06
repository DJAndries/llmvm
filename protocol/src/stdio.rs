use std::{
    env,
    error::Error,
    future::Future,
    marker::PhantomData,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::{
    io::{stdin, stdout, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::Mutex,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tower::{timeout::Timeout, Service};

use crate::{
    Backend, BackendGenerationRequest, BackendGenerationResponse, Core, GenerationRequest,
    GenerationResponse, ProtocolError, ProtocolErrorType,
};

const STDIO_COMMAND_TIMEOUT_SECS: u64 = 120;

#[derive(Serialize, Deserialize)]
pub enum BackendRequest {
    Generation(BackendGenerationRequest),
}

#[derive(Serialize, Deserialize)]
pub enum BackendResponse {
    Generation(BackendGenerationResponse),
}

#[derive(Serialize, Deserialize)]
pub enum CoreRequest {
    Generation(GenerationRequest),
}

#[derive(Serialize, Deserialize)]
pub enum CoreResponse {
    Generation(GenerationResponse),
}

pub struct BackendService<B>
where
    B: Backend,
{
    backend: Arc<B>,
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
    type Response = BackendResponse;
    type Error = Box<dyn Error + Send + Sync + 'static>;

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

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
}

impl<C> Service<CoreRequest> for CoreService<C>
where
    C: Core + 'static,
{
    type Response = CoreResponse;
    type Error = Box<dyn Error + Send + Sync + 'static>;

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
            }?)
        })
    }

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
}

#[derive(Clone, Debug, Error, Serialize, Deserialize)]
#[error("{description}")]
pub struct StdioError {
    pub error_type: ProtocolErrorType,
    pub description: String,
}

impl From<Box<ProtocolError>> for StdioError {
    fn from(error: Box<ProtocolError>) -> Self {
        Self {
            error_type: error.error_type,
            description: error.error.to_string(),
        }
    }
}

fn serialize_payload<R: Serialize>(payload: &R) -> String {
    let mut serialized = serde_json::to_string(payload).unwrap();
    serialized.push_str("\n");
    serialized
}

pub struct StdioServer<Request, Response, S>
where
    Request: Serialize + DeserializeOwned,
    Response: Serialize + DeserializeOwned,
    S: Service<Request, Response = Response, Error = Box<dyn Error + Send + Sync + 'static>>,
{
    service: Timeout<S>,
    stdin: BufReader<Stdin>,
    stdout: Stdout,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

impl<Request, Response, S> StdioServer<Request, Response, S>
where
    Request: Serialize + DeserializeOwned,
    Response: Serialize + DeserializeOwned,
    S: Service<Request, Response = Response, Error = Box<dyn Error + Send + Sync + 'static>>,
{
    pub fn new(service: S) -> Self {
        Self {
            service: Timeout::new(service, Duration::from_secs(STDIO_COMMAND_TIMEOUT_SECS)),
            stdin: BufReader::new(stdin()),
            stdout: stdout(),
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        }
    }

    async fn output_result(
        &mut self,
        result: Result<Response, Box<ProtocolError>>,
    ) -> std::io::Result<()> {
        let result = result.map_err(|e| StdioError::from(e));
        let serialized_response = serialize_payload(&result);
        self.stdout
            .write_all(serialized_response.as_bytes())
            .await?;
        Ok(())
    }

    pub async fn run(&mut self) -> std::io::Result<()> {
        loop {
            let mut serialized_request = String::new();
            self.stdin.read_line(&mut serialized_request).await?;
            let request: Request = match serde_json::from_str(&serialized_request) {
                Ok(request) => request,
                Err(e) => {
                    self.output_result(Err(Box::new(ProtocolError::new(
                        ProtocolErrorType::BadRequest,
                        e.into(),
                    ))))
                    .await?;
                    continue;
                }
            };

            let response = self.service.call(request).await.map_err(|e| {
                match e.downcast::<ProtocolError>() {
                    Ok(e) => e,
                    Err(e) => Box::new(ProtocolError::new(ProtocolErrorType::Internal, e)),
                }
                .into()
            });

            self.output_result(response).await?;
        }
    }
}

struct ChildStdio {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct StdioClient<Request, Response>
where
    Request: Serialize + DeserializeOwned,
    Response: Serialize + DeserializeOwned,
{
    _child: Child,
    child_stdio: Arc<Mutex<ChildStdio>>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

impl<Request, Response> Service<Request> for StdioClient<Request, Response>
where
    Request: Serialize + DeserializeOwned + Send + Sync + 'static,
    Response: Serialize + DeserializeOwned + Send + Sync,
{
    type Response = Result<Response, StdioError>;
    type Error = Box<dyn Error + Send + Sync + 'static>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let child_stdio = self.child_stdio.clone();
        Box::pin(async move {
            let mut child_stdio = child_stdio.lock().await;
            let serialized_request = serialize_payload(&request);
            child_stdio
                .stdin
                .write_all(serialized_request.as_bytes())
                .await?;
            let mut serialized_response = String::new();
            child_stdio
                .stdout
                .read_line(&mut serialized_response)
                .await?;
            Ok(serde_json::from_str(&serialized_response)?)
        })
    }
}

impl<Request, Response> StdioClient<Request, Response>
where
    Request: Serialize + DeserializeOwned,
    Response: Serialize + DeserializeOwned,
{
    pub async fn new(program: &str, args: &[String]) -> std::io::Result<Timeout<Self>> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(Timeout::new(
            Self {
                _child: child,
                child_stdio: Arc::new(Mutex::new(ChildStdio { stdin, stdout })),
                request_phantom: Default::default(),
                response_phantom: Default::default(),
            },
            Duration::from_secs(STDIO_COMMAND_TIMEOUT_SECS),
        ))
    }
}
