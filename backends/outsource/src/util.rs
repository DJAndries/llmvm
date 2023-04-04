use reqwest::{Response, Url};
use std::env;

use crate::{OutsourceError, Result};

pub fn get_host_and_api_key(
    api_key_env_key: &str,
    specified_api_key: Option<String>,
    api_host_env_key: &str,
    default_api_host: &str,
) -> Result<(Url, String)> {
    let host = Url::parse(
        env::var(api_host_env_key)
            .unwrap_or(default_api_host.to_string())
            .as_str(),
    )
    .map_err(|_| OutsourceError::HostURLParse)?;
    let api_key = specified_api_key
        .or(env::var(api_key_env_key).ok())
        .ok_or(OutsourceError::APIKeyNotDefined)?;
    Ok((host, api_key))
}

pub async fn check_status_code(response: Response) -> Result<Response> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        return Err(OutsourceError::BadHttpStatusCode { status, body });
    }
    Ok(response)
}
