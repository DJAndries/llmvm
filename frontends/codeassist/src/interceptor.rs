use anyhow::{anyhow, Result};
use llmvm_protocol::stdio::{CoreRequest, CoreResponse, StdioClient};
use lsp_types::{
    request::{CodeActionRequest, ExecuteCommand, Initialize, Request},
    CodeActionOrCommand, CodeActionParams, CodeActionResponse, Command, ExecuteCommandParams,
    InitializeResult, Url,
};
use serde_json::Value;
use tokio::{sync::mpsc, task::JoinHandle};
use tower::{buffer::Buffer, timeout::Timeout};
use tracing::error;

use crate::{
    complete::CodeCompleteTask,
    jsonrpc::{JsonRpcMessage, JsonRpcRequest, JsonRpcResponse},
    lsp::{CodeCompleteParams, LspMessage, CODE_COMPLETE_COMMAND_ID},
    service::{LspMessageInfo, LspMessageService, LspMessageTrx},
    snippet::SnippetFetcher,
};

const CORE_SERVICE_REQUEST_BOUND: usize = 32;

pub struct LspInterceptor {
    service_rx: mpsc::UnboundedReceiver<LspMessageTrx>,
    service_tx: Option<mpsc::UnboundedSender<LspMessageTrx>>,

    passthrough_service: LspMessageService,

    llmvm_core_service: Buffer<Timeout<StdioClient<CoreRequest, CoreResponse>>, CoreRequest>,
}

impl LspInterceptor {
    pub fn new(
        passthrough_service: LspMessageService,
        llmvm_core_service: Timeout<StdioClient<CoreRequest, CoreResponse>>,
    ) -> Self {
        let (service_tx, service_rx) = mpsc::unbounded_channel();
        Self {
            service_rx,
            service_tx: Some(service_tx),
            passthrough_service,
            llmvm_core_service: Buffer::new(llmvm_core_service, CORE_SERVICE_REQUEST_BOUND),
        }
    }

    pub fn get_service(&mut self) -> LspMessageService {
        LspMessageService::new(
            self.service_tx
                .take()
                .expect("service tx should be available"),
        )
    }

    fn transform_initialize_response(message: &LspMessage) -> Result<LspMessage> {
        let mut result = message.get_result::<InitializeResult>()?;
        let command_options = result
            .capabilities
            .execute_command_provider
            .get_or_insert_with(|| Default::default());
        command_options
            .commands
            .push(CODE_COMPLETE_COMMAND_ID.to_string());
        eprintln!("{:?}", result);
        let mut message = message.clone();
        message.set_result(result)?;
        Ok(message)
    }

    fn transform_code_action_response(
        origin_request: &JsonRpcRequest,
        message: &LspMessage,
    ) -> Result<LspMessage> {
        let request_params = serde_json::from_value::<CodeActionParams>(
            origin_request
                .params
                .clone()
                .ok_or(anyhow!("code action request does not have params"))?,
        )?;
        let mut result = message.get_result::<CodeActionResponse>()?;
        result.push(CodeActionOrCommand::Command(Command {
            title: "Complete code via LLM (openai/gpt-3.5-turbo)".to_string(),
            command: CODE_COMPLETE_COMMAND_ID.to_string(),
            arguments: Some(vec![serde_json::to_value(CodeCompleteParams {
                text_document: request_params.text_document,
                range: request_params.range,
            })?]),
        }));
        let mut message = message.clone();
        message.set_result(result)?;
        Ok(message)
    }

    fn handle_code_complete_command(&self, message: &LspMessage) -> Result<Option<Value>> {
        let mut params = message.get_request_params::<ExecuteCommandParams>()?;
        let llmvm_core_service = self.llmvm_core_service.clone();
        let passthrough_service = self.passthrough_service.clone();
        let code_complete_params = serde_json::from_value(params.arguments.pop().ok_or(
            anyhow!("code complete params must be specified with request"),
        )?)?;
        eprintln!("code complete command");
        tokio::spawn(async move {
            let code_complete = CodeCompleteTask::new(
                llmvm_core_service,
                passthrough_service,
                code_complete_params,
            );
            if let Err(e) = code_complete.run().await {
                error!("code complete task failed: {}", e);
            }
        });
        Ok(None)
    }

    fn handle_intercept(&self, msg_info: LspMessageInfo) -> LspMessage {
        let result_option = match msg_info.origin_request {
            Some(origin_request) => match origin_request.method.as_str() {
                CodeActionRequest::METHOD => Some(Self::transform_code_action_response(
                    &origin_request,
                    &msg_info.message,
                )),
                Initialize::METHOD => Some(Self::transform_initialize_response(&msg_info.message)),
                _ => None,
            },
            None => match &msg_info.message.payload {
                JsonRpcMessage::Request(req) => match req.method.as_str() {
                    ExecuteCommand::METHOD => Some(Ok(LspMessage::new_response::<ExecuteCommand>(
                        self.handle_code_complete_command(&msg_info.message),
                        req.id.clone(),
                    ))),
                    _ => None,
                },
                _ => None,
            },
        };
        if let Some(result) = result_option {
            match result {
                Ok(message) => return message,
                Err(e) => error!("intercept failed: {}", e),
            }
        }
        msg_info.message
    }

    pub async fn run(&mut self) {
        while let Some((msg_info, resp_tx)) = self.service_rx.recv().await {
            resp_tx
                .send(Some(self.handle_intercept(msg_info)))
                .expect("tried to send response back to caller");
        }
    }
}
