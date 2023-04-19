use reqwest::Response;

use crate::{OutsourceError, Result};

pub async fn check_status_code(response: Response) -> Result<Response> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        return Err(OutsourceError::BadHttpStatusCode { status, body });
    }
    Ok(response)
}

pub fn get_api_key(config_api_key: Option<&String>) -> Result<&str> {
    config_api_key
        .map(|k| k.as_str())
        .ok_or(OutsourceError::APIKeyNotDefined)
}
