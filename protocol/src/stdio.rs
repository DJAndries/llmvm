use futures::{
    stream::{pending, FuturesUnordered},
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
use tokio::{
    io::{stdin, stdout, AsyncBufReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::{
        mpsc::{self},
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
    services::{ServiceError, ServiceFuture, ServiceResponse},
    util::parse_from_value,
    BackendGenerationRequest, BackendGenerationResponse, GenerationRequest, GenerationResponse,
    NotificationStream, ProtocolError, ProtocolErrorType, SerializableProtocolError,
    COMMAND_TIMEOUT_SECS,
};

const GENERATION_METHOD: &str = "generation";
const GENERATION_STREAM_METHOD: &str = "generation_stream";
const INIT_PROJECT_METHOD: &str = "init_project";

// TODO: move these to lib/services
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
    InitProject,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum CoreResponse {
    Generation(GenerationResponse),
    GenerationStream(GenerationResponse),
    InitProject,
}

pub trait RequestJsonRpcConvert<Request> {
    fn from_jsonrpc_request(
        value: JsonRpcRequest,
    ) -> Result<Option<Request>, SerializableProtocolError>;

    fn into_jsonrpc_request(&self) -> JsonRpcRequest;
}

pub trait ResponseJsonRpcConvert<Request, Response> {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &Request,
    ) -> Result<Option<Response>, SerializableProtocolError>;

    fn into_jsonrpc_message(result: Result<Response, ProtocolError>, id: Value) -> JsonRpcMessage;
}

impl RequestJsonRpcConvert<CoreRequest> for CoreRequest {
    fn from_jsonrpc_request(
        value: JsonRpcRequest,
    ) -> Result<Option<Self>, SerializableProtocolError> {
        Ok(Some(match value.method.as_str() {
            GENERATION_METHOD => CoreRequest::Generation(value.parse_params()?),
            GENERATION_STREAM_METHOD => CoreRequest::GenerationStream(value.parse_params()?),
            INIT_PROJECT_METHOD => CoreRequest::InitProject,
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_request(&self) -> JsonRpcRequest {
        let (method, params) = match self {
            CoreRequest::Generation(request) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
            CoreRequest::GenerationStream(request) => (
                GENERATION_STREAM_METHOD,
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
    ) -> Result<Option<Self>, SerializableProtocolError> {
        match value {
            JsonRpcMessage::Response(resp) => {
                let result = resp.get_result()?;
                Ok(Some(match original_request {
                    CoreRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                    CoreRequest::InitProject => Self::InitProject,
                    _ => return Ok(None),
                }))
            }
            JsonRpcMessage::Notification(resp) => {
                let result = resp.get_result()?;
                Ok(Some(match original_request {
                    CoreRequest::GenerationStream(_) => Self::Generation(parse_from_value(result)?),
                    _ => return Ok(None),
                }))
            }
            _ => Ok(None),
        }
    }

    fn into_jsonrpc_message(
        result: Result<CoreResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcMessage {
        let mut is_notification = false;
        let result = result.map(|response| match response {
            CoreResponse::Generation(response) => serde_json::to_value(response).unwrap(),
            CoreResponse::GenerationStream(response) => {
                is_notification = true;
                serde_json::to_value(response).unwrap()
            }
            CoreResponse::InitProject => Value::Null,
        });
        match is_notification {
            true => JsonRpcNotification::new_with_result_params(result, id.to_string()).into(),
            false => JsonRpcResponse::new(result, id).into(),
        }
    }
}

impl RequestJsonRpcConvert<BackendRequest> for BackendRequest {
    fn from_jsonrpc_request(
        value: JsonRpcRequest,
    ) -> Result<Option<Self>, SerializableProtocolError> {
        Ok(Some(match value.method.as_str() {
            GENERATION_METHOD => BackendRequest::Generation(value.parse_params()?),
            GENERATION_STREAM_METHOD => BackendRequest::GenerationStream(value.parse_params()?),
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_request(&self) -> JsonRpcRequest {
        let (method, params) = match &self {
            BackendRequest::Generation(generation_response) => (
                GENERATION_METHOD,
                Some(serde_json::to_value(generation_response).unwrap()),
            ),
            BackendRequest::GenerationStream(request) => (
                GENERATION_STREAM_METHOD,
                Some(serde_json::to_value(request).unwrap()),
            ),
        };
        JsonRpcRequest::new(method.to_string(), params)
    }
}

impl ResponseJsonRpcConvert<BackendRequest, BackendResponse> for BackendResponse {
    fn from_jsonrpc_message(
        value: JsonRpcMessage,
        original_request: &BackendRequest,
    ) -> Result<Option<Self>, SerializableProtocolError> {
        Ok(Some(match value {
            JsonRpcMessage::Response(resp) => {
                let result = resp.get_result()?;
                match original_request {
                    BackendRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
                    _ => return Ok(None),
                }
            }
            JsonRpcMessage::Notification(resp) => {
                let result = resp.get_result()?;
                match original_request {
                    BackendRequest::GenerationStream(_) => {
                        Self::Generation(parse_from_value(result)?)
                    }
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        }))
    }

    fn into_jsonrpc_message(
        result: Result<BackendResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcMessage {
        let mut is_notification = false;
        let result = result
            .map(|response| match response {
                BackendResponse::Generation(response) => serde_json::to_value(response).unwrap(),
                BackendResponse::GenerationStream(response) => {
                    is_notification = true;
                    serde_json::to_value(response).unwrap()
                }
            })
            .map_err(|e| e.into());
        match is_notification {
            true => JsonRpcNotification::new_with_result_params(result, id.to_string()).into(),
            false => JsonRpcResponse::new(result, id).into(),
        }
    }
}

fn serialize_payload<R: Serialize>(payload: &R) -> String {
    let mut serialized = serde_json::to_string(payload).unwrap();
    serialized.push_str("\n");
    serialized
}

struct IdentifiedNotification<Response> {
    id: u64,
    result: Option<Result<Response, ProtocolError>>,
}

struct ServerNotificationLink<Response> {
    id: u64,
    stream: NotificationStream<Response>,
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
    Request: RequestJsonRpcConvert<Request> + Send,
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
    Request: RequestJsonRpcConvert<Request> + Send + 'static,
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
                    match Request::from_jsonrpc_request(jsonrpc_request) {
                        Err(e) => {
                            error!("could not derive request enum from json rpc request: {e}");
                            return;
                        }
                        Ok(request) => match request {
                            None => {
                                error!("unknown json rpc request received");
                                return;
                            }
                            Some(request) => (self.service.call(request), id),
                        },
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
                        notification_streams
                            .lock()
                            .await
                            .insert(id, ServerNotificationLink { id, stream });
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
    Request: RequestJsonRpcConvert<Request> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send,
{
    request: Request,
    response_tx: oneshot::Sender<Result<ServiceResponse<Response>, SerializableProtocolError>>,
}

struct ClientNotificationLink<Request, Response> {
    request: Request,
    notification_tx: UnboundedSender<Result<Response, ProtocolError>>,
}

pub struct StdioClient<Request, Response>
where
    Request: RequestJsonRpcConvert<Request> + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    _child: Arc<Child>,
    to_child_tx: UnboundedSender<ClientRequestTrx<Request, Response>>,
}

impl<Request, Response> Clone for StdioClient<Request, Response>
where
    Request: RequestJsonRpcConvert<Request> + Send + 'static,
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
    Request: RequestJsonRpcConvert<Request> + Send + 'static,
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
                .map_err(|_| SerializableProtocolError {
                    error_type: ProtocolErrorType::Internal,
                    description: "should be able to send stdio request to comm task".to_string(),
                })?;
            Ok(response_rx.await.map_err(|_| SerializableProtocolError {
                error_type: ProtocolErrorType::Internal,
                description: "should be able to recv response for stdio request from comm task"
                    .to_string(),
            })??)
        })
    }
}

impl<Request, Response> StdioClient<Request, Response>
where
    Request: RequestJsonRpcConvert<Request> + Send + 'static,
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
                            let mut jsonrpc_request = req_trx.request.into_jsonrpc_request();
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
                                        error: Box::new(SerializableProtocolError {
                                            error_type: ProtocolErrorType::BadRequest,
                                            description: "client does not support serving requests".to_string()
                                        })
                                    }), request.id).into()).await,
                                    JsonRpcMessage::Response(response) => match pending_reqs.remove(&serde_json::from_value(response.id.clone()).unwrap_or_default()) {
                                        None => warn!("received response with unknown id, ignoring"),
                                        Some(trx) => {
                                            let result = match Response::from_jsonrpc_message(response.into(), &trx.request) {
                                                Ok(response) => match response {
                                                    None => {
                                                        error!("unknown json rpc notification type received");
                                                        return;
                                                    },
                                                    Some(response) => Ok(ServiceResponse::Single(response))
                                                },
                                                Err(e) => Err(e.into())
                                            };
                                            trx.response_tx.send(result).ok();
                                        }
                                    },
                                    JsonRpcMessage::Notification(notification) => {
                                        let id = notification.method.parse::<u64>().unwrap_or_default();
                                        if let Some(trx) = pending_reqs.remove(&id) {
                                            let (notification_tx, notification_rx) = mpsc::unbounded_channel();
                                            trx.response_tx.send(Ok(ServiceResponse::Multiple(UnboundedReceiverStream::new(notification_rx).boxed()))).ok();
                                            notification_links.insert(id, ClientNotificationLink {
                                                request: trx.request,
                                                notification_tx
                                            });
                                        }
                                        match notification_links.get(&id) {
                                            None => warn!("received notification with unknown id, ignoring"),
                                            Some(link) => match notification.params.is_some() {
                                                true => {
                                                    let result = match Response::from_jsonrpc_message(notification.into(), &link.request) {
                                                        Ok(notification) => match notification {
                                                            None => {
                                                                error!("unknown json rpc notification type received");
                                                                return;
                                                            },
                                                            Some(notification) => Ok(notification)
                                                        },
                                                        Err(e) => Err(e.into())
                                                    };
                                                    link.notification_tx.send(result).ok();
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
