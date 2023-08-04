use reqwest::Response;

use crate::{OutsourceError, Result};

/// Checks the status code of a [`Response`] and returns
/// an [`OutsourceError::BadHttpStatusCode`] if it's unsuccessful.
/// Otherwise, the response will be returned.
pub async fn check_status_code(response: Response) -> Result<Response> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        return Err(OutsourceError::BadHttpStatusCode { status, body });
    }
    Ok(response)
}

/// Checks if the `config_api_key` is present and returns
/// an [`OutsourceError::APIKeyNotDefined`] if not present.
/// Otherwise, the API key will be returned.
pub fn get_api_key(config_api_key: Option<&String>) -> Result<&str> {
    config_api_key
        .map(|k| k.as_str())
        .ok_or(OutsourceError::APIKeyNotDefined)
}
