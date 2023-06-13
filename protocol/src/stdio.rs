use futures::{
    stream::{pending, FuturesUnordered, Map, SelectAll},
    Stream, StreamExt,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    marker::PhantomData,
    path::Path,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{stdin, stdout, AsyncBufReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::{
        mpsc::{self, UnboundedReceiver},
        oneshot, Mutex,
    },
};
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc::UnboundedSender,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, warn};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tower::{timeout::Timeout, Service};

use crate::{
    jsonrpc::{
        JsonRpcErrorCode, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    },
    services::{ServiceError, ServiceFuture, ServiceNotificationStream, ServiceResponse},
    BackendGenerationRequest, BackendGenerationResponse, GenerationRequest, GenerationResponse,
    ProtocolError, ProtocolErrorType, COMMAND_TIMEOUT_SECS,
};

const GENERATION_METHOD: &str = "generation";
const INIT_PROJECT_METHOD: &str = "init_project";

// TODO: move these to lib/services
#[derive(Clone, Serialize, Deserialize)]
pub enum BackendRequest {
    Generation(BackendGenerationRequest),
}

#[derive(Serialize, Deserialize)]
pub enum BackendResponse {
    Generation(BackendGenerationResponse),
}

#[derive(Clone, Serialize, Deserialize)]
pub enum CoreRequest {
    Generation(GenerationRequest),
    InitProject,
}

#[derive(Serialize, Deserialize)]
pub enum CoreResponse {
    Generation(GenerationResponse),
    InitProject,
}

// TODO: move to jsonrpc and make it a method of JsonRpcRequest
fn parse_request_from_jsonrpc_request<R: DeserializeOwned>(
    value: JsonRpcRequest,
) -> Result<R, StdioError> {
    let params = value.params.ok_or_else(|| StdioError {
        error_type: ProtocolErrorType::BadRequest,
        description: "missing parameters".to_string(),
    })?;

    serde_json::from_value::<R>(params).map_err(|error| StdioError {
        error_type: ProtocolErrorType::BadRequest,
        description: error.to_string(),
    })
}

fn get_result_from_jsonrpc_response(value: JsonRpcResponse) -> Result<Value, StdioError> {
    if let Some(error) = value.error {
        let jsonrpc_error_type = JsonRpcErrorCode::from(error.code);
        return Err(StdioError {
            error_type: jsonrpc_error_type.into(),
            description: error.message,
        });
    }
    value.result.ok_or(StdioError {
        error_type: ProtocolErrorType::BadRequest,
        description: "result not found in response".to_string(),
    })
}

fn parse_from_value<R: DeserializeOwned>(value: Value) -> Result<R, StdioError> {
    serde_json::from_value::<R>(value).map_err(|error| StdioError {
        error_type: ProtocolErrorType::BadRequest,
        description: error.to_string(),
    })
}

pub trait ResponseJsonRpcConvert<Request, Response> {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &Request,
    ) -> Result<Response, StdioError>;

    fn into_jsonrpc_message(result: Result<Response, ProtocolError>, id: Value) -> JsonRpcMessage;
}

impl TryFrom<JsonRpcRequest> for CoreRequest {
    type Error = StdioError;

    fn try_from(value: JsonRpcRequest) -> Result<Self, StdioError> {
        match value.method.as_str() {
            GENERATION_METHOD => Ok(CoreRequest::Generation(parse_request_from_jsonrpc_request(
                value,
            )?)),
            INIT_PROJECT_METHOD => Ok(CoreRequest::InitProject),
            _ => Err(StdioError {
                error_type: ProtocolErrorType::BadRequest,
                description: "unknown request type".to_string(),
            }),
        }
    }
}
impl Into<JsonRpcRequest> for CoreRequest {
    fn into(self) -> JsonRpcRequest {
        let (method, params) = match self {
            CoreRequest::Generation(request) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::InitProject => (INIT_PROJECT_METHOD, None),
        };
        JsonRpcRequest::new(method.to_string(), params)
    }
}

impl ResponseJsonRpcConvert<CoreRequest, CoreResponse> for CoreResponse {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &CoreRequest,
    ) -> Result<Self, StdioError> {
        match value {
            JsonRpcMessage::Response(resp) => {
                let result = get_result_from_jsonrpc_response(resp)?;
                Ok(match original_request {
                    CoreRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                    CoreRequest::InitProject => Self::InitProject,
                })
            }
            _ => Err(StdioError {
                error_type: ProtocolErrorType::BadRequest,
                description: "unknown response message type".to_string(),
            }),
        }
    }

    fn into_jsonrpc_message(
        result: Result<CoreResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcMessage {
        let result = result
            .map(|response| match response {
                CoreResponse::Generation(response) => serde_json::to_value(response).unwrap(),
                CoreResponse::InitProject => Value::Null,
            })
            .map_err(|e| e.into());
        JsonRpcResponse::new(result, id).into()
    }
}

impl TryFrom<JsonRpcRequest> for BackendRequest {
    type Error = StdioError;

    fn try_from(value: JsonRpcRequest) -> Result<Self, StdioError> {
        match value.method.as_str() {
            GENERATION_METHOD => Ok(BackendRequest::Generation(
                parse_request_from_jsonrpc_request(value)?,
            )),
            _ => Err(StdioError {
                error_type: ProtocolErrorType::BadRequest,
                description: "unknown request type".to_string(),
            }),
        }
    }
}
impl Into<JsonRpcRequest> for BackendRequest {
    fn into(self) -> JsonRpcRequest {
        let (method, params) = match self {
            BackendRequest::Generation(generation_response) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(generation_response).unwrap()),
            ),
        };
        JsonRpcRequest::new(method.to_string(), params)
    }
}

impl ResponseJsonRpcConvert<BackendRequest, BackendResponse> for BackendResponse {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &BackendRequest,
    ) -> Result<Self, StdioError> {
        match value {
            JsonRpcMessage::Response(resp) => {
                let result = get_result_from_jsonrpc_response(resp)?;
                Ok(match original_request {
                    BackendRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                })
            }
            _ => Err(StdioError {
                error_type: ProtocolErrorType::BadRequest,
                description: "unknown response message type".to_string(),
            }),
        }
    }

    fn into_jsonrpc_message(
        result: Result<BackendResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcMessage {
        let result = result
            .map(|response| match response {
                BackendResponse::Generation(response) => serde_json::to_value(response).unwrap(),
            })
            .map_err(|e| e.into());
        JsonRpcResponse::new(result, id).into()
    }
}

fn serialize_payload<R: Serialize>(payload: &R) -> String {
    let mut serialized = serde_json::to_string(payload).unwrap();
    serialized.push_str("\n");
    serialized
}

#[derive(Clone, Debug, Error, Serialize, Deserialize)]
#[error("{description}")]
pub struct StdioError {
    pub error_type: ProtocolErrorType,
    pub description: String,
}

impl From<ProtocolError> for StdioError {
    fn from(error: ProtocolError) -> Self {
        Self {
            error_type: error.error_type,
            description: error.error.to_string(),
        }
    }
}

struct IdentifiedNotification<Response> {
    id: u64,
    result: Option<Result<Response, ServiceError>>,
}

struct ServerNotificationLink<Response> {
    id: u64,
    stream: Pin<ServiceNotificationStream<Response>>,
}

impl<Response> Stream for ServerNotificationLink<Response> {
    type Item = IdentifiedNotification<Response>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.stream.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(Some(IdentifiedNotification {
                id: self.id,
                result,
            })),
        }
    }
}

pub struct StdioServer<Request, Response, S>
where
    Request: TryFrom<JsonRpcRequest, Error = StdioError> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + 'static,
{
    service: Timeout<S>,
    stdin: BufReader<Stdin>,
    stdout: Arc<Mutex<Stdout>>,
    notification_streams: Arc<Mutex<HashMap<u64, ServerNotificationLink<Response>>>>,
    request_phantom: PhantomData<Request>,
}

impl<Request, Response, S> StdioServer<Request, Response, S>
where
    Request: TryFrom<JsonRpcRequest, Error = StdioError> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + 'static,
{
    pub fn new(service: S) -> Self {
        let new = Self {
            service: Timeout::new(service, Duration::from_secs(COMMAND_TIMEOUT_SECS)),
            stdin: BufReader::new(stdin()),
            stdout: Arc::new(Mutex::new(stdout())),
            notification_streams: Default::default(),
            request_phantom: Default::default(),
        };
        // insert dummy notification stream so that tokio::select (in main loop)
        // does not immediately return if no streams exist
        new.notification_streams.blocking_lock().insert(
            u64::MAX,
            ServerNotificationLink {
                id: u64::MAX,
                stream: Box::pin(pending()),
            },
        );
        new
    }

    async fn output_message(stdout: &Mutex<Stdout>, message: JsonRpcMessage) {
        let serialized_message = serialize_payload(&message);
        stdout
            .lock()
            .await
            .write_all(serialized_message.as_bytes())
            .await
            .ok();
    }

    fn handle_request(&mut self, serialized_request: String) {
        let stdout = self.stdout.clone();
        let notification_streams = self.notification_streams.clone();

        let value: Value = serde_json::from_str(&serialized_request).unwrap_or_default();
        let (result_future, id) = match JsonRpcMessage::try_from(value) {
            Err(e) => {
                error!("could not parse json rpc message from client: {e}, request: {serialized_request}");
                return;
            }
            Ok(message) => match message {
                JsonRpcMessage::Request(jsonrpc_request) => {
                    let id = jsonrpc_request.id.as_u64().unwrap_or_default();
                    match Request::try_from(jsonrpc_request) {
                        Err(e) => {
                            error!("could not derive request enum from json rpc request: {e}");
                            return;
                        }
                        Ok(request) => (self.service.call(request), id),
                    }
                }
                _ => {
                    error!("ignoring non-request json rpc message from client");
                    return;
                }
            },
        };
        tokio::spawn(async move {
            let result = result_future.await;
            match result {
                Ok(response) => match response {
                    ServiceResponse::Single(response) => {
                        Self::output_message(
                            stdout.as_ref(),
                            Response::into_jsonrpc_message(Ok(response), id.into()),
                        )
                        .await;
                    }
                    ServiceResponse::Multiple(stream) => {
                        notification_streams.lock().await.insert(
                            id,
                            ServerNotificationLink {
                                id,
                                stream: Pin::from(stream),
                            },
                        );
                    }
                },
                Err(e) => {
                    Self::output_message(
                        stdout.as_ref(),
                        Response::into_jsonrpc_message(Err(e.into()), id.into()),
                    )
                    .await
                }
            }
        });
    }

    pub async fn run(mut self) -> std::io::Result<()> {
        loop {
            let mut serialized_request = String::new();
            let mut notification_streams = self.notification_streams.lock().await;
            let mut notification_futures = notification_streams
                .values_mut()
                .map(|s| s.next())
                .collect::<FuturesUnordered<_>>();
            tokio::select! {
                read_result = self.stdin.read_line(&mut serialized_request) => {
                    drop(notification_futures);
                    drop(notification_streams);
                    if read_result? == 0 {
                        break;
                    }
                    self.handle_request(serialized_request);
                },
                id_notification = notification_futures.next() => {
                    let id_notification = id_notification.unwrap().unwrap();
                    match id_notification.result {
                        Some(result) => {
                            Self::output_message(self.stdout.as_ref(), Response::into_jsonrpc_message(result.map_err(|e| e.into()), id_notification.id.into())).await;
                        },
                        None => {
                            Self::output_message(self.stdout.as_ref(), JsonRpcNotification::new(id_notification.id.to_string(), None).into()).await;
                            drop(notification_futures);
                            notification_streams.remove(&id_notification.id);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

struct ClientRequestTrx<Request, Response>
where
    Request: Into<JsonRpcRequest> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send,
{
    request: Request,
    response_tx: oneshot::Sender<Result<ServiceResponse<Response>, StdioError>>,
}

struct ClientNotificationLink<Request, Response> {
    request: Request,
    notification_tx: UnboundedSender<Result<Response, ServiceError>>,
}

pub struct StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    _child: Arc<Child>,
    to_child_tx: UnboundedSender<ClientRequestTrx<Request, Response>>,
}

impl<Request, Response> Clone for StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            _child: self._child.clone(),
            to_child_tx: self.to_child_tx.clone(),
        }
    }
}

impl<Request, Response> Service<Request> for StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    type Response = ServiceResponse<Response>;
    type Error = ServiceError;
    type Future = ServiceFuture<ServiceResponse<Response>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let to_child_tx = self.to_child_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = oneshot::channel();
            to_child_tx
                .send(ClientRequestTrx {
                    request,
                    response_tx,
                })
                .map_err(|_| StdioError {
                    error_type: ProtocolErrorType::Internal,
                    description: "should be able to send stdio request to comm task".to_string(),
                })?;
            Ok(response_rx.await.map_err(|_| StdioError {
                error_type: ProtocolErrorType::Internal,
                description: "should be able to recv response for stdio request from comm task"
                    .to_string(),
            })??)
        })
    }
}

impl<Request, Response> StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    async fn output_message(stdin: &mut ChildStdin, message: JsonRpcMessage) {
        let serialized_response = serialize_payload(&message);
        stdin.write_all(serialized_response.as_bytes()).await.ok();
    }

    fn start_comm_task(
        mut stdin: ChildStdin,
        mut stdout: BufReader<ChildStdout>,
    ) -> UnboundedSender<ClientRequestTrx<Request, Response>> {
        let (to_child_tx, mut to_child_rx) =
            mpsc::unbounded_channel::<ClientRequestTrx<Request, Response>>();
        let mut notification_links = HashMap::new();
        tokio::spawn(async move {
            let mut last_req_id = 0u64;
            let mut pending_reqs: HashMap<u64, ClientRequestTrx<Request, Response>> =
                HashMap::new();
            loop {
                let mut stdout_message = String::new();
                tokio::select! {
                    req_trx = to_child_rx.recv() => match req_trx {
                        None => return,
                        Some(req_trx) => {
                            let mut jsonrpc_request: JsonRpcRequest = req_trx.request.clone().into();
                            let id = last_req_id + 1;
                            jsonrpc_request.id = serde_json::to_value(id).unwrap();

                            last_req_id = id;
                            pending_reqs.insert(id, req_trx);

                            Self::output_message(&mut stdin, jsonrpc_request.into()).await;
                        }
                    },
                    result = stdout.read_line(&mut stdout_message) => match result {
                        Err(e) => error!("StdioClient i/o error reading line from stdout: {}" ,e),
                        Ok(bytes_read) => {
                            if bytes_read == 0 {
                                return;
                            }
                            match JsonRpcMessage::try_from(serde_json::from_str::<Value>(&stdout_message).unwrap_or_default()) {
                                Err(e) => error!("failed to parse message from server: {}", e),
                                Ok(message) => match message {
                                    JsonRpcMessage::Request(request) => Self::output_message(&mut stdin, JsonRpcResponse::new(Err(ProtocolError {
                                        error_type: ProtocolErrorType::BadRequest,
                                        error: Box::new(StdioError {
                                            error_type: ProtocolErrorType::BadRequest,
                                            description: "client does not support serving requests".to_string()
                                        })
                                    }), request.id).into()).await,
                                    JsonRpcMessage::Response(response) => match pending_reqs.remove(&serde_json::from_value(response.id.clone()).unwrap_or_default()) {
                                        None => warn!("received response with unknown id, ignoring"),
                                        Some(trx) => {
                                            trx.response_tx.send(Response::from_jsonrpc_message(response.into(), &trx.request).map(|r| ServiceResponse::Single(r))).ok();
                                        }
                                    },
                                    JsonRpcMessage::Notification(notification) => {
                                        let id = notification.method.parse::<u64>().unwrap_or_default();
                                        if let Some(trx) = pending_reqs.remove(&id) {
                                            let (notification_tx, notification_rx) = mpsc::unbounded_channel();
                                            trx.response_tx.send(Ok(ServiceResponse::Multiple(Box::new(UnboundedReceiverStream::new(notification_rx))))).ok();
                                            notification_links.insert(id, ClientNotificationLink {
                                                request: trx.request,
                                                notification_tx
                                            });
                                        }
                                        match notification_links.get(&id) {
                                            None => warn!("received notification with unknown id, ignoring"),
                                            Some(link) => match notification.params.is_some() {
                                                true => {
                                                    link.notification_tx.send(Response::from_jsonrpc_message(notification.into(), &link.request).map_err(|e| e.into())).ok();
                                                },
                                                false => {
                                                    notification_links.remove(&id);
                                                    pending_reqs.remove(&id);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        to_child_tx
    }

    pub async fn new(
        bin_path: Option<&str>,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<Self> {
        let program_with_bin_path = bin_path.map(|bin_path| {
            Path::new(bin_path)
                .join(program)
                .to_str()
                .expect("command name with bin path should convert to string")
                .to_string()
        });
        let mut child = Command::new(
            program_with_bin_path
                .as_ref()
                .map(|v| v.as_str())
                .unwrap_or(program),
        )
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let to_child_tx = Self::start_comm_task(stdin, stdout);
        Ok(Self {
            _child: Arc::new(child),
            to_child_tx,
        })
    }
}
