use lsp_types::{CodeActionOrCommand, CodeActionResponse, Command, InitializeResult};
use tokio::sync::mpsc;

use crate::{
    jsonrpc::{JsonRpcMessage, JsonRpcRequest, JsonRpcResponse},
    lsp::{LspMessage, CODE_ACTION_METHOD, CODE_COMPLETE_COMMAND_ID, INITIALIZE_METHOD},
    service::{LspMessageInfo, LspMessageService, LspMessageTrx},
};

pub struct LspInterceptor {
    service_rx: mpsc::UnboundedReceiver<LspMessageTrx>,
    service_tx: Option<mpsc::UnboundedSender<LspMessageTrx>>,

    passthrough_service: LspMessageService,
}

impl LspInterceptor {
    pub fn new(passthrough_service: LspMessageService) -> Self {
        let (service_tx, service_rx) = mpsc::unbounded_channel();
        Self {
            service_rx,
            service_tx: Some(service_tx),
            passthrough_service,
        }
    }

    pub fn get_service(&mut self) -> LspMessageService {
        LspMessageService::new(
            self.service_tx
                .take()
                .expect("service tx should be available"),
        )
    }

    fn transform_initialize_response(
        mut message: LspMessage,
    ) -> Result<Option<LspMessage>, serde_json::Error> {
        Ok(Some(match message.payload {
            JsonRpcMessage::Response(mut response) => {
                let mut result: InitializeResult =
                    serde_json::from_value(response.result.clone().unwrap_or_default())?;
                let command_options = result
                    .capabilities
                    .execute_command_provider
                    .get_or_insert_with(|| Default::default());
                command_options
                    .commands
                    .push(CODE_COMPLETE_COMMAND_ID.to_string());
                response.result = Some(serde_json::to_value(result)?);
                message.payload = JsonRpcMessage::Response(response);
                message
            }
            _ => message,
        }))
    }

    fn transform_code_action_response(
        mut message: LspMessage,
    ) -> Result<Option<LspMessage>, serde_json::Error> {
        Ok(Some(match message.payload {
            JsonRpcMessage::Response(mut response) => {
                let mut result: CodeActionResponse =
                    serde_json::from_value(response.result.clone().unwrap_or_default())?;
                result.push(CodeActionOrCommand::Command(Command {
                    title: "Complete code via LLM (openai/gpt-3.5-turbo)".to_string(),
                    command: CODE_COMPLETE_COMMAND_ID.to_string(),
                    arguments: None,
                }));
                response.result = Some(serde_json::to_value(result)?);
                message.payload = JsonRpcMessage::Response(response);
                message
            }
            _ => message,
        }))
    }

    fn handle_intercept(&self, msg_info: LspMessageInfo) -> Option<LspMessage> {
        if let Some(origin_request) = msg_info.origin_request {
            // TODO: log json error if need be
            return match origin_request.method.as_str() {
                CODE_ACTION_METHOD => Self::transform_code_action_response(msg_info.message),
                INITIALIZE_METHOD => Self::transform_initialize_response(msg_info.message),
                _ => Ok(Some(msg_info.message)),
            }
            .ok()
            .unwrap_or_default();
        }
        Some(msg_info.message)
    }

    pub async fn run(&mut self) {
        while let Some((msg_info, resp_tx)) = self.service_rx.recv().await {
            resp_tx
                .send(self.handle_intercept(msg_info))
                .expect("tried to send response back to caller");
        }
    }
}
