use std::{collections::HashSet, sync::Arc};

use anyhow::{anyhow, Result};
use llmvm_protocol::{
    jsonrpc::{JsonRpcMessage, JsonRpcRequest},
    service::{BoxedService, CoreRequest, CoreResponse},
};
use lsp_types::{
    request::{CodeActionRequest, ExecuteCommand, Initialize, Request},
    CodeActionOrCommand, CodeActionParams, CodeActionResponse, Command, ExecuteCommandParams,
    InitializeParams, InitializeResult, Location, ServerCapabilities, Url,
};

use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error};

use crate::{
    complete::{CodeCompleteTask, HashableLocation},
    content::ContentManager,
    lsp::{
        LspMessage, CODE_COMPLETE_COMMAND_ID, MANUAL_CONTEXT_ADD_COMMAND_ID,
        NEW_CHAT_THREAD_COMMAND_ID,
    },
    service::{LspMessageInfo, LspMessageService, LspMessageTrx},
    CodeAssistConfig,
};

pub struct LspAdapter {
    config: Arc<CodeAssistConfig>,

    service_rx: mpsc::UnboundedReceiver<LspMessageTrx>,
    service_tx: Option<mpsc::UnboundedSender<LspMessageTrx>>,

    passthrough_service: LspMessageService,
    server_capabilities: Option<Arc<ServerCapabilities>>,

    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,

    current_thread_id: Arc<Mutex<Option<String>>>,

    root_uri: Option<Url>,
    complete_task_last_id: usize,

    content_manager: Arc<Mutex<ContentManager>>,

    queued_random_context_locations: HashSet<HashableLocation>,
}

impl LspAdapter {
    pub fn new(
        config: Arc<CodeAssistConfig>,
        passthrough_service: LspMessageService,
        llmvm_core_service: BoxedService<CoreRequest, CoreResponse>,
    ) -> Self {
        let (service_tx, service_rx) = mpsc::unbounded_channel();
        Self {
            config,
            service_rx,
            service_tx: Some(service_tx),
            passthrough_service,
            server_capabilities: None,
            llmvm_core_service: Arc::new(Mutex::new(llmvm_core_service)),
            current_thread_id: Default::default(),
            root_uri: None,
            complete_task_last_id: 0,
            content_manager: Default::default(),
            queued_random_context_locations: Default::default(),
        }
    }

    pub fn get_service(&mut self) -> LspMessageService {
        LspMessageService::new(
            self.service_tx
                .take()
                .expect("service tx should be available"),
        )
    }

    fn inspect_initialize_request(&mut self, message: &LspMessage) -> Result<Option<LspMessage>> {
        let request = message.get_params::<InitializeParams>()?;
        self.root_uri = request.root_uri;
        Ok(None)
    }

    fn transform_initialize_response(
        &mut self,
        message: &LspMessage,
    ) -> Result<Option<LspMessage>> {
        let mut result = message.get_result::<InitializeResult>()?;
        self.server_capabilities = Some(Arc::new(result.capabilities.clone()));
        let command_options = result
            .capabilities
            .execute_command_provider
            .get_or_insert_with(|| Default::default());
        command_options
            .commands
            .push(CODE_COMPLETE_COMMAND_ID.to_string());
        let mut message = message.clone();
        message.set_result(result)?;
        Ok(Some(message))
    }

    fn transform_code_action_response(
        &self,
        origin_request: &JsonRpcRequest,
        message: &LspMessage,
    ) -> Result<Option<LspMessage>> {
        let request_params = serde_json::from_value::<CodeActionParams>(
            origin_request
                .params
                .clone()
                .ok_or(anyhow!("code action request does not have params"))?,
        )?;
        let result_value = message.get_result::<serde_json::Value>()?;
        let mut result = match result_value.is_null() {
            true => Vec::new(),
            false => serde_json::from_value::<CodeActionResponse>(result_value)?,
        };
        let location_args = Some(vec![serde_json::to_value(Location::new(
            request_params.text_document.uri.clone(),
            request_params.range,
        ))?]);
        result.push(CodeActionOrCommand::Command(Command {
            title: "Complete code via LLM".to_string(),
            command: CODE_COMPLETE_COMMAND_ID.to_string(),
            arguments: location_args.clone(),
        }));
        result.push(CodeActionOrCommand::Command(Command {
            title: "Add context to LLM code complete".to_string(),
            command: MANUAL_CONTEXT_ADD_COMMAND_ID.to_string(),
            arguments: location_args,
        }));
        if self.config.use_chat_threads {
            result.push(CodeActionOrCommand::Command(Command {
                title: "Use new LLM chat thread".to_string(),
                command: NEW_CHAT_THREAD_COMMAND_ID.to_string(),
                arguments: Some(Vec::new()),
            }));
        }
        let mut message = message.clone();
        message.set_result(result)?;
        Ok(Some(message))
    }

    fn get_code_location_from_params(params: &mut ExecuteCommandParams) -> Result<Location> {
        Ok(serde_json::from_value(params.arguments.pop().ok_or(
            anyhow!("code complete params must be specified with request"),
        )?)?)
    }

    fn handle_code_complete_command(&mut self, mut params: ExecuteCommandParams) -> Result<()> {
        let config = self.config.clone();
        let llmvm_core_service = self.llmvm_core_service.clone();
        let passthrough_service = self.passthrough_service.clone();
        let server_capabilities = self.server_capabilities.clone();
        let root_uri = self.root_uri.clone();
        let content_manager = self.content_manager.clone();
        let code_location = Self::get_code_location_from_params(&mut params)?;
        let task_id = self.complete_task_last_id;
        let random_context_locations = self
            .queued_random_context_locations
            .drain()
            .map(|v| v.0)
            .collect();
        self.complete_task_last_id += 1;
        let current_thread_id = self.current_thread_id.clone();
        tokio::spawn(async move {
            let code_complete = CodeCompleteTask::new(
                config,
                llmvm_core_service,
                passthrough_service,
                server_capabilities.as_ref().map(|v| v.as_ref()),
                root_uri,
                content_manager,
                code_location,
                task_id,
                random_context_locations,
                current_thread_id,
            );
            code_complete.run().await.ok();
        });
        Ok(())
    }

    fn handle_add_context_command(&mut self, mut params: ExecuteCommandParams) -> Result<()> {
        let code_location = Self::get_code_location_from_params(&mut params)?;
        self.queued_random_context_locations
            .insert(HashableLocation(code_location));
        debug!(
            "random context added, current random context len = {}",
            self.queued_random_context_locations.len()
        );
        Ok(())
    }

    async fn handle_doc_sync_notification(
        &self,
        message: &LspMessage,
    ) -> Result<Option<LspMessage>> {
        self.content_manager
            .lock()
            .await
            .handle_doc_sync_notification(message)?;
        Ok(None)
    }

    async fn handle_intercept(&mut self, msg_info: LspMessageInfo) -> Option<LspMessage> {
        let result = match msg_info.origin_request {
            Some(origin_request) => match origin_request.method.as_str() {
                CodeActionRequest::METHOD => {
                    self.transform_code_action_response(&origin_request, &msg_info.message)
                }
                Initialize::METHOD => self.transform_initialize_response(&msg_info.message),
                _ => Ok(None),
            },
            None => match &msg_info.message.payload {
                JsonRpcMessage::Request(req) => match req.method.as_str() {
                    ExecuteCommand::METHOD => {
                        let command_result =
                            match msg_info.message.get_params::<ExecuteCommandParams>().ok() {
                                Some(params) => match params.command.as_str() {
                                    CODE_COMPLETE_COMMAND_ID => {
                                        self.handle_code_complete_command(params)
                                    }
                                    MANUAL_CONTEXT_ADD_COMMAND_ID => {
                                        self.handle_add_context_command(params)
                                    }
                                    NEW_CHAT_THREAD_COMMAND_ID => {
                                        *self.current_thread_id.lock().await = None;
                                        Ok(())
                                    }
                                    _ => Ok(()),
                                },
                                None => Ok(()),
                            }
                            .map(|_| None);
                        Ok(Some(LspMessage::new_response::<ExecuteCommand>(
                            command_result,
                            req.id.clone(),
                        )))
                    }
                    Initialize::METHOD => self.inspect_initialize_request(&msg_info.message),
                    _ => Ok(None),
                },
                JsonRpcMessage::Notification(_) => {
                    self.handle_doc_sync_notification(&msg_info.message).await
                }
                _ => Ok(None),
            },
        };
        match result {
            Ok(new_message) => new_message,
            Err(e) => {
                error!("intercept failed: {}", e);
                None
            }
        }
    }

    pub async fn run(&mut self) {
        while let Some((msg_info, resp_tx)) = self.service_rx.recv().await {
            resp_tx
                .send(self.handle_intercept(msg_info).await)
                .expect("tried to send response back to caller");
        }
    }
}
