use reqwest::Url;
use std::env;

use crate::{OutsourceError, Result};

pub fn get_host_and_api_key(
    api_key_env_key: &str,
    api_host_env_key: &str,
    default_api_host: &str,
) -> Result<(Url, String)> {
    let host = Url::parse(
        env::var(api_host_env_key)
            .unwrap_or(default_api_host.to_string())
            .as_str(),
    )
    .map_err(|_| OutsourceError::HostURLParse)?;
    let api_key = env::var(api_key_env_key).map_err(|_| OutsourceError::APIKeyNotDefined)?;
    Ok((host, api_key))
}
