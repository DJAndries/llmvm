use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use lsp_types::{request::Request, Range, TextDocumentIdentifier};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::jsonrpc::JsonRpcMessage;

pub const CONTENT_LENGTH_HEADER: &str = "Content-Length";

pub const CODE_COMPLETE_COMMAND_ID: &str = "llmvm-codeassist/complete";

#[derive(Clone, Debug)]
pub struct LspMessage {
    pub headers: HashMap<String, String>,
    pub payload: JsonRpcMessage,
}

impl LspMessage {
    pub fn new_request<R: Request>(request: R::Params) -> Result<Self, serde_json::Error> {
        Ok(Self {
            headers: HashMap::new(),
            payload: JsonRpcMessage::new_request(
                R::METHOD.to_string(),
                Some(serde_json::to_value(request)?),
            ),
        })
    }

    pub fn new_response<R: Request>(result: Result<R::Result>, id: Value) -> Self {
        Self {
            headers: HashMap::new(),
            payload: JsonRpcMessage::new_response(
                result.map(|v| serde_json::to_value(v).unwrap()),
                id,
            ),
        }
    }

    pub fn get_result<R: DeserializeOwned>(&self) -> Result<R> {
        Ok(match &self.payload {
            JsonRpcMessage::Response(resp) => match &resp.error {
                Some(error) => bail!(
                    "json rpc error encountered while converting into result: {}",
                    error.message
                ),
                None => {
                    let result = resp
                        .result
                        .as_ref()
                        .ok_or(anyhow!("expected result in lsp message response"))?;
                    serde_json::from_value(result.clone())?
                }
            },
            _ => bail!("payload should include response when converting lsp message to result"),
        })
    }

    pub fn set_result<R: Serialize>(&mut self, result: R) -> Result<()> {
        match &mut self.payload {
            JsonRpcMessage::Response(resp) => {
                resp.result = Some(serde_json::to_value(result)?);
            }
            _ => bail!("payload should include response when setting lsp message result"),
        }
        Ok(())
    }

    pub fn get_request_params<R: DeserializeOwned>(&self) -> Result<R> {
        Ok(match &self.payload {
            JsonRpcMessage::Request(req) => {
                let result = req
                    .params
                    .as_ref()
                    .ok_or(anyhow!("expected params in lsp message request"))?;
                serde_json::from_value(result.clone())?
            }
            _ => bail!("payload should include request when converting lsp message to params"),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeCompleteParams {
    pub text_document: TextDocumentIdentifier,
    pub range: Range,
}
