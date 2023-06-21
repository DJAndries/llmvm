use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{ProtocolErrorType, SerializableProtocolError};

pub fn parse_from_value<R: DeserializeOwned>(value: Value) -> Result<R, SerializableProtocolError> {
    serde_json::from_value::<R>(value).map_err(|error| SerializableProtocolError {
        error_type: ProtocolErrorType::BadRequest,
        description: error.to_string(),
    })
}
