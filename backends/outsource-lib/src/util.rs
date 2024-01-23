use llmvm_protocol::ModelDescription;
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
pub fn get_api_key(config_api_key: Option<&str>) -> Result<&str> {
    config_api_key.ok_or(OutsourceError::APIKeyNotDefined)
}

pub fn get_openai_api_key<'a>(
    api_key: Option<&'a str>,
    is_config_custom_endpoint_present: bool,
    model_description: &ModelDescription,
) -> Result<&'a str> {
    let api_key_result = get_api_key(api_key);

    Ok(
        if is_config_custom_endpoint_present || model_description.endpoint.is_some() {
            // allow absence of API key if custom endpoint is defined
            api_key_result.unwrap_or("")
        } else {
            api_key_result?
        },
    )
}
