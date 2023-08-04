use std::str::Utf8Error;

use handlebars::RenderError;
use llmvm_protocol::{
    error::{ProtocolErrorType, SerializableProtocolError},
    ProtocolError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("failed to start stdio backend: {0}")]
    StdioBackendStart(std::io::Error),
    #[error("template not found")]
    TemplateNotFound,
    #[error("utf8 error: {0}")]
    Utf8Error(#[from] Utf8Error),
    #[error("unable to load app data, could not find user home folder")]
    UserHomeNotFound,
    #[error("json serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),
    #[error("toml serialization error: {0}")]
    TomlSerialization(#[from] toml::de::Error),
    #[error("thread not found")]
    ThreadNotFound,
    #[error("preset not found")]
    PresetNotFound,
    #[error("failed to parse model name")]
    ModelDescriptionParse,
    #[error("backend error (via stdio): {0}")]
    BackendStdio(#[from] SerializableProtocolError),
    #[error("backend error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("tempate render error: {0}")]
    TemplateRender(#[from] RenderError),
    #[error("missing generation parameters")]
    MissingParameters,
    #[error("missing required generation parameter: {0}")]
    MissingParameter(&'static str),
    #[error("failed to create http backend service")]
    HttpServiceCreate,
    #[error("unexpected service response type")]
    UnexpectedServiceResponse,
}

impl Into<ProtocolError> for CoreError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            CoreError::IO(_) => ProtocolErrorType::Internal,
            CoreError::StdioBackendStart(_) => ProtocolErrorType::Internal,
            CoreError::TemplateNotFound => ProtocolErrorType::BadRequest,
            CoreError::Utf8Error(_) => ProtocolErrorType::BadRequest,
            CoreError::UserHomeNotFound => ProtocolErrorType::Internal,
            CoreError::JsonSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::TomlSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::ThreadNotFound => ProtocolErrorType::NotFound,
            CoreError::PresetNotFound => ProtocolErrorType::BadRequest,
            CoreError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            CoreError::BackendStdio(error) => error.error_type.clone(),
            CoreError::Protocol(error) => error.error_type.clone(),
            CoreError::TemplateRender(_) => ProtocolErrorType::BadRequest,
            CoreError::MissingParameters => ProtocolErrorType::BadRequest,
            CoreError::MissingParameter(_) => ProtocolErrorType::BadRequest,
            CoreError::HttpServiceCreate => ProtocolErrorType::Internal,
            CoreError::UnexpectedServiceResponse => ProtocolErrorType::Internal,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}
