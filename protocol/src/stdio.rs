use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    future::Future,
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
    io::{stdin, stdout, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::{
        mpsc::{self, UnboundedReceiver},
        oneshot, Mutex,
    },
};
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc::UnboundedSender,
};
use tracing::{error, warn};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tower::{timeout::Timeout, Service};

use crate::{
    jsonrpc::{
        JsonRpcErrorCode, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    },
    services::{ServiceError, ServiceFuture},
    BackendGenerationRequest, BackendGenerationResponse, GenerationRequest, GenerationResponse,
    ProtocolError, ProtocolErrorType, COMMAND_TIMEOUT_SECS,
};

const GENERATION_METHOD: &str = "generation";
const INIT_PROJECT_METHOD: &str = "init_project";

// TODO: move these to lib
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
    fn from_jsonrpc_response(
        value: JsonRpcResponse,
        original_request: &Request,
    ) -> Result<Response, StdioError>;

    fn into_jsonrpc_response(result: Result<Response, ProtocolError>, id: Value)
        -> JsonRpcResponse;
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
    fn from_jsonrpc_response(
        value: JsonRpcResponse,
        original_request: &CoreRequest,
    ) -> Result<Self, StdioError> {
        let result = get_result_from_jsonrpc_response(value)?;
        Ok(match original_request {
            CoreRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
            CoreRequest::InitProject => CoreResponse::InitProject,
        })
    }

    fn into_jsonrpc_response(
        result: Result<CoreResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcResponse {
        let result = result
            .map(|response| match response {
                CoreResponse::Generation(response) => serde_json::to_value(response).unwrap(),
                CoreResponse::InitProject => Value::Null,
            })
            .map_err(|e| e.into());
        JsonRpcResponse::new(result, id)
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
    fn from_jsonrpc_response(
        value: JsonRpcResponse,
        original_request: &BackendRequest,
    ) -> Result<Self, StdioError> {
        let result = get_result_from_jsonrpc_response(value)?;
        Ok(match original_request {
            BackendRequest::Generation(_) => Self::Generation(parse_from_value(result)?),
        })
    }

    fn into_jsonrpc_response(
        result: Result<BackendResponse, ProtocolError>,
        id: Value,
    ) -> JsonRpcResponse {
        let result = result
            .map(|response| match response {
                BackendResponse::Generation(response) => serde_json::to_value(response).unwrap(),
            })
            .map_err(|e| e.into());
        JsonRpcResponse::new(result, id)
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

pub struct StdioServer<Request, Response, S>
where
    Request: TryFrom<JsonRpcRequest, Error = StdioError> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Clone
        + 'static,
{
    service: Timeout<S>,
    stdin: BufReader<Stdin>,
    stdout: Arc<Mutex<Stdout>>,
    message_tx: Option<UnboundedSender<JsonRpcMessage>>,
    message_rx: UnboundedReceiver<JsonRpcMessage>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

impl<Request, Response, S> StdioServer<Request, Response, S>
where
    Request: TryFrom<JsonRpcRequest, Error = StdioError> + Send,
    Response: ResponseJsonRpcConvert<Request, Response> + Send,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Clone
        + 'static,
{
    pub fn new(service: S) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Self {
            service: Timeout::new(service, Duration::from_secs(COMMAND_TIMEOUT_SECS)),
            stdin: BufReader::new(stdin()),
            stdout: Arc::new(Mutex::new(stdout())),
            message_tx: Some(message_tx),
            message_rx,
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        }
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

    fn handle_request(&self, serialized_request: String) {
        let mut service = self.service.clone();
        let stdout = self.stdout.clone();

        tokio::spawn(async move {
            let value: Value = serde_json::from_str(&serialized_request).unwrap_or_default();
            let (result, id) = match JsonRpcMessage::try_from(value) {
                Err(e) => {
                    error!("could not parse json rpc message from client: {e}, request: {serialized_request}");
                    return;
                }
                Ok(message) => match message {
                    JsonRpcMessage::Request(jsonrpc_request) => {
                        let id = jsonrpc_request.id.clone();
                        match Request::try_from(jsonrpc_request) {
                            Err(e) => {
                                error!("could not derive request enum from json rpc request: {e}");
                                return;
                            }
                            Ok(request) => (
                                service.call(request).await.map_err(|e| {
                                    match e.downcast::<ProtocolError>() {
                                        Ok(e) => *e,
                                        Err(e) => {
                                            ProtocolError::new(ProtocolErrorType::Internal, e)
                                        }
                                    }
                                }),
                                id,
                            ),
                        }
                    }
                    _ => {
                        error!("ignoring non-request json rpc message from client");
                        return;
                    }
                },
            };
            Self::output_message(
                stdout.as_ref(),
                Response::into_jsonrpc_response(result, id).into(),
            )
            .await
        });
    }

    pub fn get_message_sender(&mut self) -> UnboundedSender<JsonRpcMessage> {
        self.message_tx
            .take()
            .expect("message sender should be present")
    }

    pub async fn run(mut self) -> std::io::Result<()> {
        loop {
            let mut serialized_request = String::new();
            tokio::select! {
                read_result = self.stdin.read_line(&mut serialized_request) => {
                    if read_result? == 0 {
                        break;
                    }
                    self.handle_request(serialized_request);
                },
                message = self.message_rx.recv() => if let Some(message) = message {
                    Self::output_message(self.stdout.as_ref(), message).await;
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
    response_tx: oneshot::Sender<Result<Response, StdioError>>,
}

struct NotificationReceiveLink {
    method: String,
    notification_tx: UnboundedSender<JsonRpcNotification>,
}

pub struct StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    _child: Arc<Child>,
    to_child_tx: UnboundedSender<ClientRequestTrx<Request, Response>>,
    notification_recv_link_tx: UnboundedSender<NotificationReceiveLink>,
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
            notification_recv_link_tx: self.notification_recv_link_tx.clone(),
        }
    }
}

impl<Request, Response> Service<Request> for StdioClient<Request, Response>
where
    Request: Into<JsonRpcRequest> + Clone + Send + 'static,
    Response: ResponseJsonRpcConvert<Request, Response> + Send + 'static,
{
    type Response = Result<Response, StdioError>;
    type Error = Box<dyn Error + Send + Sync + 'static>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

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
            })?)
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
    ) -> (
        UnboundedSender<ClientRequestTrx<Request, Response>>,
        UnboundedSender<NotificationReceiveLink>,
    ) {
        let (to_child_tx, mut to_child_rx) =
            mpsc::unbounded_channel::<ClientRequestTrx<Request, Response>>();
        let (notification_recv_link_tx, mut notification_recv_link_rx) =
            mpsc::unbounded_channel::<NotificationReceiveLink>();
        tokio::spawn(async move {
            let mut last_req_id = 0u64;
            let mut pending_reqs: HashMap<u64, ClientRequestTrx<Request, Response>> =
                HashMap::new();
            let mut notification_txs: HashMap<String, UnboundedSender<JsonRpcNotification>> =
                HashMap::new();
            loop {
                notification_txs.retain(|_, tx| !tx.is_closed());
                let mut stdout_message = String::new();
                tokio::select! {
                    recv_link = notification_recv_link_rx.recv() => if let Some(recv_link) = recv_link {
                        notification_txs.insert(recv_link.method, recv_link.notification_tx);
                    },
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
                                            trx.response_tx.send(Response::from_jsonrpc_response(response, &trx.request)).ok();
                                        }
                                    },
                                    JsonRpcMessage::Notification(notification) => if let Some(tx) = notification_txs.get(&notification.method) {
                                        tx.send(notification).ok();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        (to_child_tx, notification_recv_link_tx)
    }

    pub fn get_notification_receiver(
        &self,
        method: String,
    ) -> Option<UnboundedReceiver<JsonRpcNotification>> {
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        self.notification_recv_link_tx
            .send(NotificationReceiveLink {
                method,
                notification_tx,
            })
            .ok()
            .map(|_| notification_rx)
    }

    pub async fn new(
        bin_path: Option<&str>,
        program: &str,
        args: &[String],
    ) -> std::io::Result<Timeout<Self>> {
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
        let (to_child_tx, notification_recv_link_tx) = Self::start_comm_task(stdin, stdout);
        Ok(Timeout::new(
            Self {
                _child: Arc::new(child),
                to_child_tx,
                notification_recv_link_tx,
            },
            Duration::from_secs(COMMAND_TIMEOUT_SECS),
        ))
    }
}
