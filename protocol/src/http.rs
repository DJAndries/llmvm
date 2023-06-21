use std::{
    collections::VecDeque,
    convert::Infallible,
    marker::PhantomData,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use async_stream::stream;
use futures::stream::StreamExt;
use hyper::{
    body::{to_bytes, Body},
    header::CONTENT_TYPE,
    http::{uri::InvalidUri, HeaderValue, Request as HttpRequest},
    Method, Response as HttpResponse, StatusCode, Uri,
};
#[cfg(feature = "http-client")]
use hyper::{client::HttpConnector, Client};
#[cfg(feature = "http-server")]
use hyper::{server::conn::AddrStream, service::make_service_fn, Server};
#[cfg(feature = "http-client")]
use hyper_rustls::HttpsConnector;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tower::{timeout::Timeout, Service};
use tracing::{debug, info, warn};

use crate::{
    services::{ServiceError, ServiceFuture, ServiceResponse},
    stdio::{BackendRequest, BackendResponse, CoreRequest, CoreResponse},
    util::parse_from_value,
    HttpClientConfig, HttpServerConfig, NotificationStream, ProtocolError, ProtocolErrorType,
    SerializableProtocolError, COMMAND_TIMEOUT_SECS,
};

const API_KEY_HEADER: &str = "X-API-Key";
const GENERATE_PATH: &str = "/generate";
const GENERATE_STREAM_PATH: &str = "/generate_stream";

async fn parse_request<T: DeserializeOwned>(
    request: HttpRequest<Body>,
) -> Result<T, ProtocolError> {
    let bytes = to_bytes(request)
        .await
        .map_err(|e| ProtocolError::new(ProtocolErrorType::Internal, Box::new(e)))?;
    serde_json::from_slice(bytes.as_ref())
        .map_err(|e| ProtocolError::new(ProtocolErrorType::BadRequest, Box::new(e)))
}

async fn parse_response<T: DeserializeOwned>(
    response: HttpResponse<Body>,
) -> Result<T, ProtocolError> {
    let bytes = to_bytes(response)
        .await
        .map_err(|e| ProtocolError::new(ProtocolErrorType::Internal, Box::new(e)))?;
    parse_response_payload(bytes.as_ref())
}

fn parse_response_payload<T: DeserializeOwned>(response: &[u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(response)
        .map_err(|e| ProtocolError::new(ProtocolErrorType::BadRequest, Box::new(e)))
}

fn serialize_to_http_request<T: Serialize>(
    base_url: &Uri,
    path: &str,
    method: Method,
    request: &T,
) -> Result<HttpRequest<Body>, ProtocolError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|e| ProtocolError::new(ProtocolErrorType::Internal, Box::new(e)))?;
    let url = Uri::builder()
        .scheme(
            base_url
                .scheme()
                .expect("base url should contain scheme")
                .clone(),
        )
        .authority(
            base_url
                .authority()
                .expect("base url should contain authority")
                .clone(),
        )
        .path_and_query(path)
        .build()
        .expect("should be able to build url");
    Ok(HttpRequest::builder()
        .method(method)
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .body(bytes.into())
        .expect("should be able to create http request"))
}

fn serialize_response<T: Serialize>(response: &T) -> Result<Vec<u8>, ProtocolError> {
    serde_json::to_vec(response)
        .map_err(|e| ProtocolError::new(ProtocolErrorType::Internal, Box::new(e)))
}

fn serialize_to_http_response<T: Serialize>(
    response: &T,
    status: StatusCode,
) -> Result<HttpResponse<Body>, ProtocolError> {
    let bytes = serialize_response(response)?;
    Ok(HttpResponse::builder()
        .header(CONTENT_TYPE, "application/json")
        .status(status)
        .body(bytes.into())
        .expect("should be able to create http response"))
}

pub fn notification_sse_response<Request, Response>(
    notification_stream: NotificationStream<Response>,
) -> HttpResponse<Body>
where
    Request: Clone,
    Response: ResponseHttpConvert<Request, Response> + 'static,
{
    let payload_stream = notification_stream.map(|result| {
        let payload =
            HttpNotificationPayload::from(result.map_err(|e| ProtocolError::from(e)).and_then(
                |response| {
                    Response::to_http_response(ServiceResponse::Single(response)).map(|opt| {
                        opt.and_then(|response| match response {
                            ModalHttpResponse::Event(value) => Some(value),
                            _ => None,
                        })
                    })
                },
            ));
        let payload_str = serde_json::to_string(&payload)?;
        Ok::<String, serde_json::Error>(format!("data: {}\n\n", payload_str))
    });
    HttpResponse::new(Body::wrap_stream(payload_stream))
}

pub fn notification_sse_stream<Request, Response>(
    original_request: Request,
    http_response: HttpResponse<Body>,
) -> NotificationStream<Response>
where
    Request: Clone + Send + Sync + 'static,
    Response: ResponseHttpConvert<Request, Response> + Send + Sync + 'static,
{
    let mut body = http_response.into_body();
    stream! {
        let mut buffer = VecDeque::new();
        while let Some(bytes_result) = body.next().await {
            match bytes_result {
                Err(e) => {
                    let boxed_e: ServiceError = Box::new(e);
                    yield Err(boxed_e.into());
                    return;
                },
                Ok(bytes) => {
                    buffer.extend(bytes);
                }
            }
            while let Some(linebreak_pos) = buffer.iter().position(|b| b == &b'\n') {
                let line_bytes = buffer.drain(0..linebreak_pos).collect::<Vec<_>>();
                if let Ok(line) = std::str::from_utf8(&line_bytes) {
                    if !line.starts_with("data: ") {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&line[6..]) {
                        let resp = Response::from_http_response(ModalHttpResponse::Event(value), &original_request).await
                            .and_then(|response| response.ok_or_else(|| generic_error(ProtocolErrorType::NotFound)))
                            .and_then(|response| match response {
                                ServiceResponse::Single(response) => Ok(response),
                                _ => Err(generic_error(ProtocolErrorType::NotFound))
                            });
                        yield resp;
                    }
                }
                buffer.drain(0..linebreak_pos);
                buffer.remove(linebreak_pos);
            }
        }
    }.boxed()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpNotificationPayload {
    pub result: Option<Value>,
    pub error: Option<SerializableProtocolError>,
}

impl From<Result<Option<Value>, ProtocolError>> for HttpNotificationPayload {
    fn from(result: Result<Option<Value>, ProtocolError>) -> Self {
        let result =
            result.and_then(|r| r.ok_or_else(|| generic_error(ProtocolErrorType::NotFound)));
        let (result, error) = match result {
            Ok(result) => (Some(result), None),
            Err(e) => (None, Some(e.into())),
        };
        Self { result, error }
    }
}

pub enum ModalHttpResponse {
    Single(HttpResponse<Body>),
    Event(Value),
}

#[async_trait::async_trait]
pub trait RequestHttpConvert<Request> {
    async fn from_http_request(
        request: HttpRequest<Body>,
    ) -> Result<Option<Request>, ProtocolError>;

    fn to_http_request(&self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError>;
}

#[async_trait::async_trait]
pub trait ResponseHttpConvert<Request, Response>
where
    Request: Clone,
    Response: ResponseHttpConvert<Request, Response>,
{
    async fn from_http_response(
        response: ModalHttpResponse,
        original_request: &Request,
    ) -> Result<Option<ServiceResponse<Response>>, ProtocolError>;

    fn to_http_response(
        response: ServiceResponse<Response>,
    ) -> Result<Option<ModalHttpResponse>, ProtocolError>;
}

#[async_trait::async_trait]
impl RequestHttpConvert<CoreRequest> for CoreRequest {
    async fn from_http_request(request: HttpRequest<Body>) -> Result<Option<Self>, ProtocolError> {
        let request = match request.uri().path() {
            GENERATE_PATH => match request.method() == &Method::POST {
                true => CoreRequest::Generation(parse_request(request).await?),
                false => return Err(generic_error(ProtocolErrorType::HttpMethodNotAllowed).into()),
            },
            GENERATE_STREAM_PATH => match request.method() == &Method::POST {
                true => CoreRequest::GenerationStream(parse_request(request).await?),
                false => return Err(generic_error(ProtocolErrorType::HttpMethodNotAllowed).into()),
            },
            _ => return Ok(None),
        };
        Ok(Some(request))
    }

    fn to_http_request(&self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError> {
        let request = match self {
            CoreRequest::Generation(request) => {
                serialize_to_http_request(base_url, GENERATE_PATH, Method::POST, &request)?
            }
            CoreRequest::GenerationStream(request) => {
                serialize_to_http_request(base_url, GENERATE_STREAM_PATH, Method::POST, &request)?
            }
            _ => return Ok(None),
        };
        Ok(Some(request))
    }
}

#[async_trait::async_trait]
impl ResponseHttpConvert<CoreRequest, CoreResponse> for CoreResponse {
    async fn from_http_response(
        response: ModalHttpResponse,
        original_request: &CoreRequest,
    ) -> Result<Option<ServiceResponse<Self>>, ProtocolError> {
        Ok(Some(match response {
            ModalHttpResponse::Single(response) => match original_request {
                CoreRequest::Generation(_) => ServiceResponse::Single(CoreResponse::Generation(
                    parse_response(response).await?,
                )),
                CoreRequest::GenerationStream(_) => ServiceResponse::Multiple(
                    notification_sse_stream(original_request.clone(), response),
                ),
                _ => return Ok(None),
            },
            ModalHttpResponse::Event(event) => ServiceResponse::Single(match original_request {
                CoreRequest::GenerationStream(_) => {
                    CoreResponse::GenerationStream(parse_from_value(event)?)
                }
                _ => return Ok(None),
            }),
        }))
    }

    fn to_http_response(
        response: ServiceResponse<Self>,
    ) -> Result<Option<ModalHttpResponse>, ProtocolError> {
        let response = match response {
            ServiceResponse::Single(response) => match response {
                CoreResponse::Generation(response) => ModalHttpResponse::Single(
                    serialize_to_http_response(&response, StatusCode::OK)?,
                ),
                CoreResponse::GenerationStream(response) => {
                    ModalHttpResponse::Event(serde_json::to_value(response).unwrap())
                }
                _ => return Ok(None),
            },
            ServiceResponse::Multiple(stream) => {
                ModalHttpResponse::Single(notification_sse_response(stream))
            }
        };
        Ok(Some(response))
    }
}

#[async_trait::async_trait]
impl RequestHttpConvert<BackendRequest> for BackendRequest {
    async fn from_http_request(request: HttpRequest<Body>) -> Result<Option<Self>, ProtocolError> {
        let request = match request.uri().path() {
            GENERATE_PATH => match request.method() == &Method::POST {
                true => BackendRequest::Generation(parse_request(request).await?),
                false => return Err(generic_error(ProtocolErrorType::HttpMethodNotAllowed).into()),
            },
            GENERATE_STREAM_PATH => match request.method() == &Method::POST {
                true => BackendRequest::GenerationStream(parse_request(request).await?),
                false => return Err(generic_error(ProtocolErrorType::HttpMethodNotAllowed).into()),
            },
            _ => return Ok(None),
        };
        Ok(Some(request))
    }

    fn to_http_request(&self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError> {
        let request = match self {
            BackendRequest::Generation(request) => {
                serialize_to_http_request(base_url, GENERATE_PATH, Method::POST, &request)?
            }
            BackendRequest::GenerationStream(request) => {
                serialize_to_http_request(base_url, GENERATE_STREAM_PATH, Method::POST, &request)?
            }
        };
        Ok(Some(request))
    }
}

#[async_trait::async_trait]
impl ResponseHttpConvert<BackendRequest, BackendResponse> for BackendResponse {
    async fn from_http_response(
        response: ModalHttpResponse,
        original_request: &BackendRequest,
    ) -> Result<Option<ServiceResponse<Self>>, ProtocolError> {
        let response = match response {
            ModalHttpResponse::Single(response) => match original_request {
                BackendRequest::Generation(_) => ServiceResponse::Single(
                    BackendResponse::Generation(parse_response(response).await?),
                ),
                BackendRequest::GenerationStream(request) => ServiceResponse::Multiple(
                    notification_sse_stream(original_request.clone(), response),
                ),
                _ => return Ok(None),
            },
            ModalHttpResponse::Event(event) => ServiceResponse::Single(match original_request {
                BackendRequest::GenerationStream(_) => {
                    BackendResponse::GenerationStream(parse_from_value(event)?)
                }
                _ => return Ok(None),
            }),
        };
        Ok(Some(response))
    }

    fn to_http_response(
        response: ServiceResponse<Self>,
    ) -> Result<Option<ModalHttpResponse>, ProtocolError> {
        Ok(Some(match response {
            ServiceResponse::Single(response) => match response {
                BackendResponse::Generation(response) => ModalHttpResponse::Single(
                    serialize_to_http_response(&response, StatusCode::OK)?,
                ),
                BackendResponse::GenerationStream(response) => {
                    ModalHttpResponse::Event(serde_json::to_value(response).unwrap())
                }
                _ => return Ok(None),
            },
            ServiceResponse::Multiple(stream) => {
                ModalHttpResponse::Single(notification_sse_response(stream))
            }
        }))
    }
}

#[derive(Debug, Error, Serialize, Deserialize)]
#[error("{error}")]
pub struct ProtocolHttpError {
    pub error: String,
}

impl Into<StatusCode> for ProtocolErrorType {
    fn into(self) -> StatusCode {
        match self {
            ProtocolErrorType::BadRequest => StatusCode::BAD_REQUEST,
            ProtocolErrorType::Unauthorized => StatusCode::UNAUTHORIZED,
            ProtocolErrorType::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ProtocolErrorType::NotFound => StatusCode::NOT_FOUND,
            ProtocolErrorType::HttpMethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        }
    }
}

impl From<StatusCode> for ProtocolErrorType {
    fn from(code: StatusCode) -> Self {
        match code {
            StatusCode::BAD_REQUEST => ProtocolErrorType::BadRequest,
            StatusCode::UNAUTHORIZED => ProtocolErrorType::Unauthorized,
            StatusCode::INTERNAL_SERVER_ERROR => ProtocolErrorType::Internal,
            StatusCode::NOT_FOUND => ProtocolErrorType::NotFound,
            StatusCode::METHOD_NOT_ALLOWED => ProtocolErrorType::HttpMethodNotAllowed,
            _ => ProtocolErrorType::Internal,
        }
    }
}

impl Into<HttpResponse<Body>> for ProtocolError {
    fn into(self) -> HttpResponse<Body> {
        let payload = ProtocolHttpError {
            error: self.error.to_string(),
        };
        serialize_to_http_response(&payload, self.error_type.into())
            .expect("should serialize error into http response")
    }
}

fn generic_error(error_type: ProtocolErrorType) -> ProtocolError {
    let status: StatusCode = error_type.clone().into();
    let error = Box::new(ProtocolHttpError {
        error: status.to_string(),
    });
    ProtocolError { error_type, error }
}

#[cfg(feature = "http-server")]
struct HttpServerConnService<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Clone,
    Response: ResponseHttpConvert<Request, Response>,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Clone
        + 'static,
{
    config: Arc<HttpServerConfig>,
    service: Timeout<S>,
    remote_addr: Option<SocketAddr>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

#[cfg(feature = "http-server")]
impl<Request, Response, S> Service<HttpRequest<Body>>
    for HttpServerConnService<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Clone + Send,
    Response: ResponseHttpConvert<Request, Response> + Send,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Clone
        + 'static,
{
    type Response = HttpResponse<Body>;
    type Error = ServiceError;
    type Future = ServiceFuture<HttpResponse<Body>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: HttpRequest<Body>) -> Self::Future {
        let config = self.config.clone();
        let mut service = self.service.clone();
        debug!(
            "received http request from {}",
            self.remote_addr.as_ref().unwrap()
        );
        let remote_addr = self.remote_addr.take().unwrap();
        Box::pin(async move {
            if !config.api_keys.is_empty() {
                let key_header = request
                    .headers()
                    .get(API_KEY_HEADER)
                    .map(|v| v.to_str().unwrap_or_default())
                    .unwrap_or_default();
                if !config.api_keys.contains(key_header) {
                    return Ok(generic_error(ProtocolErrorType::Unauthorized).into());
                }
            }

            let uri = request.uri().to_string();
            let request_result = Request::from_http_request(request).await;
            let response = match request_result {
                Ok(request_option) => match request_option {
                    Some(request) => {
                        let response = service.call(request).await;
                        response
                            .map(|response| {
                                // Map an Ok service response into an http response
                                Response::to_http_response(response)
                                    .map(|r| r.and_then(|r| match r {
                                        ModalHttpResponse::Single(r) => Some(r),
                                        ModalHttpResponse::Event(_) => {
                                            warn!("unexpected event response returned from http response conversion, returning 404");
                                            None
                                        }
                                    }))
                                    .unwrap_or_else(|e| Some(e.into()))
                                    .unwrap_or_else(|| {
                                        generic_error(ProtocolErrorType::NotFound).into()
                                    })
                            })
                            .unwrap_or_else(|e| {
                                // Map service error into an http response
                                ProtocolError::from(e).into()
                            })
                    }
                    // If option is None, we can assume that the request resulted
                    // in Not Found
                    None => generic_error(ProtocolErrorType::NotFound).into(),
                },
                Err(e) => e.into(),
            };
            info!(
                uri = uri,
                status = response.status().to_string(),
                "handled http request from {}",
                remote_addr,
            );
            Ok(response)
        })
    }
}

#[cfg(feature = "http-server")]
pub struct HttpServer<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Clone + Send,
    Response: ResponseHttpConvert<Request, Response>,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Clone
        + 'static,
{
    config: Arc<HttpServerConfig>,
    service: Timeout<S>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

impl<Request, Response, S> HttpServer<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Send + Clone,
    Response: ResponseHttpConvert<Request, Response>,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Clone
        + 'static,
{
    pub fn new(service: S, config: HttpServerConfig) -> Self {
        Self {
            config: Arc::new(config),
            service: Timeout::new(service, Duration::from_secs(COMMAND_TIMEOUT_SECS)),
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        }
    }
}

#[cfg(feature = "http-server")]
impl<Request, Response, S> HttpServer<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Clone + Send + 'static,
    Response: ResponseHttpConvert<Request, Response> + Send + 'static,
    S: Service<
            Request,
            Response = ServiceResponse<Response>,
            Error = ServiceError,
            Future = ServiceFuture<ServiceResponse<Response>>,
        > + Send
        + Clone
        + 'static,
{
    pub async fn run(self) -> Result<(), hyper::Error> {
        let config_cl = self.config.clone();
        let service_cl = self.service.clone();
        let make_service = make_service_fn(move |conn: &AddrStream| {
            let config = config_cl.clone();
            let service = service_cl.clone();
            let remote_addr = Some(conn.remote_addr());
            async move {
                Ok::<_, Infallible>(HttpServerConnService {
                    config,
                    service,
                    remote_addr,
                    request_phantom: Default::default(),
                    response_phantom: Default::default(),
                })
            }
        });
        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.port));

        let server = Server::try_bind(&addr)?;

        info!("listening to http requests on port {}", self.config.port);

        server.serve(make_service).await
    }
}

#[cfg(feature = "http-client")]
#[derive(Clone)]
pub struct HttpClient<Request, Response>
where
    Request: RequestHttpConvert<Request> + Clone + Send + 'static,
    Response: ResponseHttpConvert<Request, Response> + Send + 'static,
{
    base_url: Arc<Uri>,
    config: Arc<HttpClientConfig>,
    client: Client<HttpsConnector<HttpConnector>>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

#[cfg(feature = "http-client")]
impl<Request, Response> HttpClient<Request, Response>
where
    Request: RequestHttpConvert<Request> + Clone + Send + 'static,
    Response: ResponseHttpConvert<Request, Response> + Send + 'static,
{
    pub fn new(config: HttpClientConfig) -> Result<Self, InvalidUri> {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder().build(https);
        let base_url = Arc::new(Uri::from_str(&config.base_url)?);
        Ok(Self {
            base_url,
            config: Arc::new(config),
            client,
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        })
    }
}

#[cfg(feature = "http-client")]
impl<Request, Response> Service<Request> for HttpClient<Request, Response>
where
    Request: RequestHttpConvert<Request> + Clone + Send + Sync + 'static,
    Response: ResponseHttpConvert<Request, Response> + Send + 'static,
{
    type Response = ServiceResponse<Response>;
    type Error = ServiceError;
    type Future = ServiceFuture<ServiceResponse<Response>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let http_request = request.to_http_request(&self.base_url);
        let mut client = self.client.clone();
        let api_key = self.config.api_key.clone();
        Box::pin(async move {
            let mut http_request =
                http_request?.ok_or_else(|| generic_error(ProtocolErrorType::NotFound))?;
            if let Some(api_key) = api_key {
                http_request
                    .headers_mut()
                    .insert(API_KEY_HEADER, HeaderValue::from_str(&api_key)?);
            }
            let response = client.call(http_request).await?;
            let status = response.status();
            if !status.is_success() {
                return Err(Box::new(ProtocolError {
                    error_type: response.status().into(),
                    error: Box::new(parse_response::<ProtocolHttpError>(response).await?),
                }))?;
            }
            let response =
                Response::from_http_response(ModalHttpResponse::Single(response), &request).await?;
            Ok(response.ok_or_else(|| generic_error(ProtocolErrorType::NotFound))?)
        })
    }
}
