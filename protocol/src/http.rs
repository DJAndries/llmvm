pub use multilink::http::*;

use multilink::{
    error::ProtocolErrorType,
    http::hyper::{Body, Method, StatusCode, Uri},
    http::util::{
        notification_sse_response, notification_sse_stream, parse_request, parse_response,
        serialize_to_http_request, serialize_to_http_response, validate_method,
    },
    util::parse_from_value,
    ProtocolError, ServiceResponse,
};
use serde_json::Value;

use crate::service::{BackendRequest, BackendResponse, CoreRequest, CoreResponse};

const GENERATE_PATH: &str = "/generate";
const GENERATE_STREAM_PATH: &str = "/generate_stream";
const GET_LAST_THREAD_INFO_METHOD: &str = "/threads/last";
const GET_THREAD_MESSAGES_METHOD_PREFIX: &str = "/threads/";
const GET_ALL_THREAD_INFOS_METHOD: &str = "/threads";

#[async_trait::async_trait]
impl RequestHttpConvert<CoreRequest> for CoreRequest {
    async fn from_http_request(request: HttpRequest<Body>) -> Result<Option<Self>, ProtocolError> {
        let path = request.uri().path();
        let request = match path {
            GENERATE_PATH => {
                validate_method(&request, Method::POST)?;
                CoreRequest::Generation(parse_request(request).await?)
            }
            GENERATE_STREAM_PATH => {
                validate_method(&request, Method::POST)?;
                CoreRequest::GenerationStream(parse_request(request).await?)
            }
            GET_ALL_THREAD_INFOS_METHOD => {
                validate_method(&request, Method::GET)?;
                CoreRequest::GetAllThreadInfos
            }
            GET_LAST_THREAD_INFO_METHOD => {
                validate_method(&request, Method::GET)?;
                CoreRequest::GetLastThreadInfo
            }
            _ => {
                if !path.starts_with(GET_THREAD_MESSAGES_METHOD_PREFIX)
                    || request.method() != &Method::GET
                    || path.split(&['/', '\\']).count() != 2
                {
                    return Ok(None);
                }
                let id = path.split(&['/', '\\']).nth(1).unwrap();
                CoreRequest::GetThreadMessages { id: id.to_string() }
            }
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
            CoreRequest::GetLastThreadInfo => serialize_to_http_request(
                base_url,
                GET_LAST_THREAD_INFO_METHOD,
                Method::GET,
                &Value::Null,
            )?,
            CoreRequest::GetAllThreadInfos => serialize_to_http_request(
                base_url,
                GET_ALL_THREAD_INFOS_METHOD,
                Method::GET,
                &Value::Null,
            )?,
            CoreRequest::GetThreadMessages { id } => {
                serialize_to_http_request(base_url, GET_ALL_THREAD_INFOS_METHOD, Method::GET, &id)?
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
                CoreRequest::GetLastThreadInfo => ServiceResponse::Single(
                    CoreResponse::GetLastThreadInfo(parse_response(response).await?),
                ),
                CoreRequest::GetAllThreadInfos => ServiceResponse::Single(
                    CoreResponse::GetAllThreadInfos(parse_response(response).await?),
                ),
                CoreRequest::GetThreadMessages { .. } => ServiceResponse::Single(
                    CoreResponse::GetThreadMessages(parse_response(response).await?),
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
                CoreResponse::GetLastThreadInfo(response) => ModalHttpResponse::Single(
                    serialize_to_http_response(&response, StatusCode::OK)?,
                ),
                CoreResponse::GetAllThreadInfos(response) => ModalHttpResponse::Single(
                    serialize_to_http_response(&response, StatusCode::OK)?,
                ),
                CoreResponse::GetThreadMessages(response) => ModalHttpResponse::Single(
                    serialize_to_http_response(&response, StatusCode::OK)?,
                ),
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
                BackendRequest::GenerationStream(_) => ServiceResponse::Multiple(
                    notification_sse_stream(original_request.clone(), response),
                ),
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
            },
            ServiceResponse::Multiple(stream) => {
                ModalHttpResponse::Single(notification_sse_response(stream))
            }
        }))
    }
}
