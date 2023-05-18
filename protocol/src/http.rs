use std::{
    convert::Infallible,
    marker::PhantomData,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use hyper::{
    body::{to_bytes, Body},
    client::HttpConnector,
    header::CONTENT_TYPE,
    http::{uri::InvalidUri, Request as HttpRequest},
    Client, Method, Response as HttpResponse, StatusCode, Uri,
};
#[cfg(feature = "http-server")]
use hyper::{server::conn::AddrStream, service::make_service_fn, Server};
#[cfg(feature = "http-client")]
use hyper_rustls::HttpsConnector;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;
use tower::{timeout::Timeout, Service};
use tracing::{debug, info};

use crate::{
    services::{ServiceError, ServiceFuture},
    stdio::{BackendRequest, BackendResponse, CoreRequest, CoreResponse},
    HttpClientConfig, HttpServerConfig, ProtocolError, ProtocolErrorType, COMMAND_TIMEOUT_SECS,
};

const GENERATE_PATH: &str = "/generate";

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
    serde_json::from_slice(bytes.as_ref())
        .map_err(|e| ProtocolError::new(ProtocolErrorType::BadRequest, Box::new(e)))
}

fn serialize_to_http_request<T: Serialize>(
    base_url: &Uri,
    path: &str,
    method: Method,
    request: T,
) -> Result<HttpRequest<Body>, ProtocolError> {
    let bytes = serde_json::to_vec(&request)
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
        .path_and_query(format!("{}{}", base_url.path(), path))
        .build()
        .expect("should be able to build url");
    Ok(HttpRequest::builder()
        .method(method)
        .uri(url)
        .header(CONTENT_TYPE, "application/json")
        .body(bytes.into())
        .expect("should be able to create http request"))
}

fn serialize_to_http_response<T: Serialize>(
    response: T,
    status: StatusCode,
) -> Result<HttpResponse<Body>, ProtocolError> {
    let bytes = serde_json::to_vec(&response)
        .map_err(|e| ProtocolError::new(ProtocolErrorType::Internal, Box::new(e)))?;
    Ok(HttpResponse::builder()
        .header(CONTENT_TYPE, "application/json")
        .status(status)
        .body(bytes.into())
        .expect("should be able to create http response"))
}

#[async_trait::async_trait]
pub trait RequestHttpConvert<R> {
    async fn from_http_request(request: HttpRequest<Body>) -> Result<Option<R>, ProtocolError>;

    fn to_http_request(self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError>;
}

#[async_trait::async_trait]
pub trait ResponseHttpConvert<R> {
    async fn from_http_response(
        request_path: &str,
        response: HttpResponse<Body>,
    ) -> Result<Option<R>, ProtocolError>;

    fn to_http_response(self) -> Result<Option<HttpResponse<Body>>, ProtocolError>;
}

#[async_trait::async_trait]
impl RequestHttpConvert<CoreRequest> for CoreRequest {
    async fn from_http_request(request: HttpRequest<Body>) -> Result<Option<Self>, ProtocolError> {
        let request = match request.uri().path() {
            GENERATE_PATH => match request.method() == &Method::POST {
                true => CoreRequest::Generation(parse_request(request).await?),
                false => return Ok(None),
            },
            _ => return Ok(None),
        };
        Ok(Some(request))
    }

    fn to_http_request(self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError> {
        let request = match self {
            CoreRequest::Generation(request) => {
                serialize_to_http_request(base_url, GENERATE_PATH, Method::POST, request)?
            }
            _ => return Ok(None),
        };
        Ok(Some(request))
    }
}

#[async_trait::async_trait]
impl ResponseHttpConvert<CoreResponse> for CoreResponse {
    async fn from_http_response(
        request_path: &str,
        response: HttpResponse<Body>,
    ) -> Result<Option<Self>, ProtocolError> {
        let response = match request_path {
            GENERATE_PATH => CoreResponse::Generation(parse_response(response).await?),
            _ => return Ok(None),
        };
        Ok(Some(response))
    }

    fn to_http_response(self) -> Result<Option<HttpResponse<Body>>, ProtocolError> {
        let response = match self {
            CoreResponse::Generation(response) => {
                serialize_to_http_response(response, StatusCode::OK)?
            }
            _ => return Ok(None),
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
            _ => return Ok(None),
        };
        Ok(Some(request))
    }

    fn to_http_request(self, base_url: &Uri) -> Result<Option<HttpRequest<Body>>, ProtocolError> {
        let request = match self {
            BackendRequest::Generation(request) => {
                serialize_to_http_request(base_url, GENERATE_PATH, Method::POST, request)?
            }
        };
        Ok(Some(request))
    }
}

#[async_trait::async_trait]
impl ResponseHttpConvert<BackendResponse> for BackendResponse {
    async fn from_http_response(
        request_path: &str,
        response: HttpResponse<Body>,
    ) -> Result<Option<Self>, ProtocolError> {
        let response = match request_path {
            GENERATE_PATH => BackendResponse::Generation(parse_response(response).await?),
            _ => return Ok(None),
        };
        Ok(Some(response))
    }

    fn to_http_response(self) -> Result<Option<HttpResponse<Body>>, ProtocolError> {
        let response = match self {
            BackendResponse::Generation(response) => {
                serialize_to_http_response(response, StatusCode::OK)?
            }
        };
        Ok(Some(response))
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

impl Into<HttpResponse<Body>> for ProtocolError {
    fn into(self) -> HttpResponse<Body> {
        let payload = ProtocolHttpError {
            error: self.error.to_string(),
        };
        serialize_to_http_response(payload, self.error_type.into())
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
    Request: RequestHttpConvert<Request>,
    Response: ResponseHttpConvert<Response>,
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
    remote_addr: Option<SocketAddr>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

#[cfg(feature = "http-server")]
impl<Request, Response, S> Service<HttpRequest<Body>>
    for HttpServerConnService<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Send,
    Response: ResponseHttpConvert<Response> + Send,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
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
        let mut service = self.service.clone();
        debug!(
            "received http request from {}",
            self.remote_addr.as_ref().unwrap()
        );
        let remote_addr = self.remote_addr.take().unwrap();
        Box::pin(async move {
            let uri = request.uri().to_string();
            let request_result = Request::from_http_request(request).await;
            let response = match request_result {
                Ok(request_option) => match request_option {
                    Some(request) => {
                        let response = service.call(request).await;
                        response
                            .map(|response| {
                                // Map an Ok service response into an http response
                                response
                                    .to_http_response()
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
    Request: RequestHttpConvert<Request>,
    Response: ResponseHttpConvert<Response>,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Clone
        + 'static,
{
    config: HttpServerConfig,
    service: Timeout<S>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

impl<Request, Response, S> HttpServer<Request, Response, S>
where
    Request: RequestHttpConvert<Request>,
    Response: ResponseHttpConvert<Response>,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Clone
        + 'static,
{
    pub fn new(service: S, config: HttpServerConfig) -> Self {
        Self {
            config,
            service: Timeout::new(service, Duration::from_secs(COMMAND_TIMEOUT_SECS)),
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        }
    }
}

#[cfg(feature = "http-server")]
impl<Request, Response, S> HttpServer<Request, Response, S>
where
    Request: RequestHttpConvert<Request> + Send + 'static,
    Response: ResponseHttpConvert<Response> + Send + 'static,
    S: Service<
            Request,
            Response = Response,
            Error = ServiceError,
            Future = ServiceFuture<Response>,
        > + Send
        + Clone
        + 'static,
{
    pub async fn run(self) -> Result<(), hyper::Error> {
        let make_service = make_service_fn(move |conn: &AddrStream| {
            let service = self.service.clone();
            let remote_addr = Some(conn.remote_addr());
            async move {
                Ok::<_, Infallible>(HttpServerConnService {
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
    Request: RequestHttpConvert<Request> + Send + 'static,
    Response: ResponseHttpConvert<Response> + Send + 'static,
{
    base_url: Arc<Uri>,
    client: Client<HttpsConnector<HttpConnector>>,
    request_phantom: PhantomData<Request>,
    response_phantom: PhantomData<Response>,
}

#[cfg(feature = "http-client")]
impl<Request, Response> HttpClient<Request, Response>
where
    Request: RequestHttpConvert<Request> + Send + 'static,
    Response: ResponseHttpConvert<Response> + Send + 'static,
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
            client,
            request_phantom: Default::default(),
            response_phantom: Default::default(),
        })
    }
}

#[cfg(feature = "http-client")]
impl<Request, Response> Service<Request> for HttpClient<Request, Response>
where
    Request: RequestHttpConvert<Request> + Send + 'static,
    Response: ResponseHttpConvert<Response> + Send + 'static,
{
    type Response = Response;
    type Error = ServiceError;
    type Future = ServiceFuture<Response>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let request = request.to_http_request(&self.base_url);
        let mut client = self.client.clone();
        Box::pin(async move {
            let request = request?.ok_or_else(|| generic_error(ProtocolErrorType::NotFound))?;
            let request_path = request.uri().path().to_string();
            let response = client.call(request).await?;
            let response = Response::from_http_response(&request_path, response).await?;
            Ok(response.ok_or_else(|| generic_error(ProtocolErrorType::NotFound))?)
        })
    }
}
