use std::collections::HashMap;

use anyhow::Result;
use llmvm_protocol::{
    jsonrpc::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse},
    tower::Service,
};
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Notification,
    },
    request::{CodeActionRequest, ExecuteCommand, Initialize, Request},
    ExecuteCommandParams,
};
use tokio::sync::oneshot;

use crate::{
    lsp::{LspMessage, CODE_COMPLETE_COMMAND_ID, MANUAL_CONTEXT_ADD_COMMAND_ID},
    service::{LspMessageInfo, LspMessageService},
};

type PendingCallMap = HashMap<String, oneshot::Sender<Option<LspMessage>>>;

pub struct InterceptorResult {
    pub forwarded_message_info: Option<LspMessageInfo>,
    pub should_exit: bool,
}

enum MessageHandleResult {
    Replacement(LspMessageInfo),
    Forward { should_exit: bool },
    DontForward,
}

pub struct LspInterceptor {
    // for pending service calls originating from the adapter
    pending_interceptor_calls: PendingCallMap,
    // for intercepted requests which will be sent to the interceptor
    // once responses have been received
    pending_intercepted_requests: HashMap<String, JsonRpcRequest>,

    adapter_service: LspMessageService,
}

impl LspInterceptor {
    pub fn new(adapter_service: LspMessageService) -> Self {
        Self {
            pending_interceptor_calls: HashMap::new(),
            pending_intercepted_requests: HashMap::new(),
            adapter_service,
        }
    }

    pub fn add_pending_interceptor_call(
        &mut self,
        request: &JsonRpcRequest,
        resp_tx: oneshot::Sender<Option<LspMessage>>,
    ) {
        self.pending_interceptor_calls
            .insert(request.id.to_string(), resp_tx);
    }

    async fn handle_response(
        &mut self,
        message_info: &LspMessageInfo,
        response: &JsonRpcResponse,
    ) -> Result<MessageHandleResult> {
        if let Some(resp_tx) = self
            .pending_interceptor_calls
            .remove(&response.id.to_string())
        {
            resp_tx
                .send(Some(message_info.message.clone()))
                .expect("tried to send response back to caller");
            return Ok(MessageHandleResult::DontForward);
        }
        if let Some(origin_request) = self
            .pending_intercepted_requests
            .remove(&response.id.to_string())
        {
            let msg_info = LspMessageInfo {
                message: message_info.message.clone(),
                to_real_server: message_info.to_real_server,
                origin_request: Some(origin_request),
            };
            if let Some(new_msg) = self.adapter_service.call(msg_info).await? {
                return Ok(MessageHandleResult::Replacement(LspMessageInfo::new(
                    new_msg,
                    message_info.to_real_server,
                )));
            }
        }
        Ok(MessageHandleResult::Forward { should_exit: false })
    }

    async fn handle_request(
        &mut self,
        message_info: &LspMessageInfo,
        request: &JsonRpcRequest,
    ) -> Result<MessageHandleResult> {
        match request.method.as_str() {
            CodeActionRequest::METHOD | Initialize::METHOD => {
                self.pending_intercepted_requests
                    .insert(request.id.to_string(), request.clone());
                if request.method == Initialize::METHOD {
                    self.adapter_service.call(message_info.clone()).await?;
                }
            }
            ExecuteCommand::METHOD => {
                if let Ok(execute_params) = serde_json::from_value::<ExecuteCommandParams>(
                    request.params.clone().unwrap_or_default(),
                ) {
                    if [CODE_COMPLETE_COMMAND_ID, MANUAL_CONTEXT_ADD_COMMAND_ID]
                        .contains(&execute_params.command.as_str())
                    {
                        if let Some(new_msg) =
                            self.adapter_service.call(message_info.clone()).await?
                        {
                            // Send the intercepted command result back to the client
                            return Ok(MessageHandleResult::Replacement(LspMessageInfo::new(
                                new_msg, false,
                            )));
                        }
                    }
                }
            }
            _ => (),
        };
        return Ok(MessageHandleResult::Forward { should_exit: false });
    }

    async fn handle_notification(
        &mut self,
        message_info: &LspMessageInfo,
        notification: &JsonRpcNotification,
    ) -> Result<MessageHandleResult> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD
            | DidChangeTextDocument::METHOD
            | DidCloseTextDocument::METHOD => {
                self.adapter_service.call(message_info.clone()).await?;
            }
            Exit::METHOD => {
                return Ok(MessageHandleResult::Forward { should_exit: true });
            }
            _ => (),
        };
        return Ok(MessageHandleResult::Forward { should_exit: false });
    }

    pub async fn maybe_intercept(
        &mut self,
        message_info: LspMessageInfo,
    ) -> Result<InterceptorResult> {
        let intercept_result = match &message_info.message.payload {
            JsonRpcMessage::Response(resp) => self.handle_response(&message_info, resp).await?,
            JsonRpcMessage::Request(req) => self.handle_request(&message_info, req).await?,
            JsonRpcMessage::Notification(notification) => {
                self.handle_notification(&message_info, notification)
                    .await?
            }
        };
        Ok(match intercept_result {
            MessageHandleResult::Replacement(message_info) => InterceptorResult {
                forwarded_message_info: Some(message_info),
                should_exit: false,
            },
            MessageHandleResult::Forward { should_exit } => InterceptorResult {
                forwarded_message_info: Some(message_info),
                should_exit,
            },
            MessageHandleResult::DontForward => InterceptorResult {
                forwarded_message_info: None,
                should_exit: false,
            },
        })
    }
}
