use std::{
    collections::{HashMap},
    pin::Pin,
};

use anyhow::Result;
use llmvm_protocol::{
    jsonrpc::{JsonRpcMessage, JsonRpcRequest},
    tower::Service,
};
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Notification,
    },
    request::{CodeActionRequest, ExecuteCommand, Initialize, Request},
    ExecuteCommandParams,
};

use serde_json::Value;
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Stdin,
        Stdout,
    },
    process::{ChildStdin, ChildStdout},
    sync::{
        mpsc::{self},
        oneshot,
    },
};
use tracing::debug;

use crate::{
    lsp::{
        LspMessage, CODE_COMPLETE_COMMAND_ID, CONTENT_LENGTH_HEADER, MANUAL_CONTEXT_ADD_COMMAND_ID,
    },
    service::{LspMessageInfo, LspMessageService, LspMessageTrx},
};

const OUR_REQ_ID_PREFIX: &str = "llmvm/";

type PendingCallMap = HashMap<String, oneshot::Sender<Option<LspMessage>>>;

pub struct LspStdioPassthrough {
    our_stdin: Pin<Box<dyn AsyncBufRead>>,
    our_stdout: Pin<Box<dyn AsyncWrite>>,
    real_server_stdin: Pin<Box<dyn AsyncWrite>>,
    real_server_stdout: Pin<Box<dyn AsyncBufRead>>,

    service_rx: mpsc::UnboundedReceiver<LspMessageTrx>,
    service_tx: Option<mpsc::UnboundedSender<LspMessageTrx>>,

    // for pending service calls from the interceptor
    pending_interceptor_calls: PendingCallMap,
    // for intercepted requests which will be sent to the interceptor
    // once responses have been received
    pending_intercepted_requests: HashMap<String, JsonRpcRequest>,

    interceptor_service: Option<LspMessageService>,

    new_request_last_id: usize,
}

impl LspStdioPassthrough {
    pub fn new(
        our_stdin: Stdin,
        our_stdout: Stdout,
        real_server_stdin: ChildStdin,
        real_server_stdout: ChildStdout,
    ) -> Self {
        let (service_tx, service_rx) = mpsc::unbounded_channel();
        Self {
            our_stdin: Box::pin(BufReader::new(our_stdin)),
            our_stdout: Box::pin(our_stdout),
            real_server_stdin: Box::pin(real_server_stdin),
            real_server_stdout: Box::pin(BufReader::new(real_server_stdout)),
            service_rx,
            service_tx: Some(service_tx),
            pending_interceptor_calls: HashMap::new(),
            pending_intercepted_requests: HashMap::new(),
            interceptor_service: None,
            new_request_last_id: 0,
        }
    }

    pub fn get_service(&mut self) -> LspMessageService {
        LspMessageService::new(
            self.service_tx
                .take()
                .expect("service tx should be available"),
        )
    }

    pub fn set_interceptor_service(&mut self, service: LspMessageService) {
        self.interceptor_service = Some(service);
    }

    async fn maybe_intercept(
        &mut self,
        mut message: LspMessage,
        mut to_real_server: bool,
    ) -> Result<(Option<LspMessageInfo>, bool)> {
        let mut should_exit = false;
        match &message.payload {
            JsonRpcMessage::Response(resp) => {
                if let Some(resp_tx) = self.pending_interceptor_calls.remove(&resp.id.to_string()) {
                    resp_tx
                        .send(Some(message))
                        .expect("tried to send response back to caller");
                    return Ok((None, false));
                }
                if let Some(origin_request) = self
                    .pending_intercepted_requests
                    .remove(&resp.id.to_string())
                {
                    if let Some(service) = self.interceptor_service.as_mut() {
                        let msg_info = LspMessageInfo {
                            message: message.clone(),
                            to_real_server,
                            origin_request: Some(origin_request),
                        };
                        if let Some(new_msg) = service.call(msg_info).await? {
                            message = new_msg;
                        }
                    }
                }
            }
            JsonRpcMessage::Request(req) => {
                match req.method.as_str() {
                    CodeActionRequest::METHOD | Initialize::METHOD => {
                        self.pending_intercepted_requests
                            .insert(req.id.to_string(), req.clone());
                        if req.method == Initialize::METHOD {
                            if let Some(service) = self.interceptor_service.as_mut() {
                                service
                                    .call(LspMessageInfo {
                                        message: message.clone(),
                                        to_real_server,
                                        origin_request: None,
                                    })
                                    .await?;
                            }
                        }
                    }
                    ExecuteCommand::METHOD => {
                        if let Ok(execute_params) = serde_json::from_value::<ExecuteCommandParams>(
                            req.params.clone().unwrap_or_default(),
                        ) {
                            if [CODE_COMPLETE_COMMAND_ID, MANUAL_CONTEXT_ADD_COMMAND_ID]
                                .contains(&execute_params.command.as_str())
                            {
                                if let Some(service) = self.interceptor_service.as_mut() {
                                    let msg_info = LspMessageInfo {
                                        message: message.clone(),
                                        to_real_server,
                                        origin_request: None,
                                    };
                                    if let Some(new_msg) = service.call(msg_info).await? {
                                        message = new_msg;
                                        to_real_server = false;
                                    }
                                }
                            }
                        }
                    }
                    _ => (),
                };
            }
            JsonRpcMessage::Notification(notification) => match notification.method.as_str() {
                DidOpenTextDocument::METHOD
                | DidChangeTextDocument::METHOD
                | DidCloseTextDocument::METHOD => {
                    if let Some(service) = self.interceptor_service.as_mut() {
                        service
                            .call(LspMessageInfo {
                                message: message.clone(),
                                to_real_server,
                                origin_request: None,
                            })
                            .await?;
                    }
                }
                Exit::METHOD => {
                    should_exit = true;
                }
                _ => (),
            },
        };
        Ok((
            Some(LspMessageInfo::new(message, to_real_server)),
            should_exit,
        ))
    }

    async fn send_message(
        output: &mut Pin<Box<dyn AsyncWrite>>,
        message: &LspMessage,
    ) -> Result<()> {
        let payload_vec = match serde_json::to_vec(&message.payload) {
            Ok(p) => p,
            // TODO: add tracing for error
            // Err(_) => return Ok(should_exit),
            Err(_) => return Ok(()),
        };
        for (key, value) in &message.headers {
            if key != CONTENT_LENGTH_HEADER {
                let header_line = format!("{}: {}\r\n", key, value);
                output.write_all(&header_line.as_bytes()).await?;
            }
        }
        let header_line = format!("{}: {}\r\n", CONTENT_LENGTH_HEADER, payload_vec.len());
        output.write_all(&header_line.as_bytes()).await?;
        output.write_all("\r\n".as_bytes()).await?;
        output.write_all(&payload_vec).await?;
        output.flush().await?;
        Ok(())
    }

    async fn handle_message(&mut self, to_real_server: bool) -> Result<bool> {
        let input = match to_real_server {
            true => &mut self.our_stdin,
            false => &mut self.real_server_stdout,
        };
        let mut content_length: Option<usize> = None;
        let mut header_line = String::new();
        let mut headers = HashMap::new();
        while input.read_line(&mut header_line).await? > 0 {
            let header_trimmed = header_line.trim();
            if header_trimmed.is_empty() {
                if headers.is_empty() {
                    return Ok(false);
                }
                break;
            }
            let mut header_split = header_trimmed.split(": ");
            let header_key = match header_split.next() {
                None => break,
                Some(key) => key,
            };
            let header_value = match header_split.next() {
                None => break,
                Some(value) => value,
            };
            if header_key == CONTENT_LENGTH_HEADER {
                content_length = match header_value.parse::<usize>() {
                    Ok(length) => Some(length),
                    Err(_) => break,
                };
            }
            headers.insert(header_key.to_string(), header_value.to_string());

            header_line.clear();
        }
        if content_length.is_none() {
            return Ok(false);
        }
        let content_length = content_length.unwrap();
        let mut payload_vec: Vec<u8> = Vec::new();
        while payload_vec.len() < content_length {
            let buffer = input.fill_buf().await?;
            if buffer.len() == 0 {
                return Ok(true);
            }
            let copy_len = if payload_vec.len() + buffer.len() <= content_length {
                buffer.len()
            } else {
                content_length - payload_vec.len()
            };
            payload_vec.extend(&buffer[..copy_len]);
            input.consume(copy_len);
        }

        let payload = match serde_json::from_slice::<Value>(&payload_vec) {
            Ok(payload) => match JsonRpcMessage::try_from(payload) {
                Ok(payload) => payload,
                Err(_) => return Ok(false),
            },
            // TODO: add tracing for error
            Err(_) => return Ok(false),
        };

        let (message_info, should_exit) = self
            .maybe_intercept(LspMessage { headers, payload }, to_real_server)
            .await?;

        if let Some(message_info) = message_info {
            let output = match message_info.to_real_server {
                true => &mut self.real_server_stdin,
                false => &mut self.our_stdout,
            };
            Self::send_message(output, &message_info.message).await?;
        }

        Ok(should_exit)
    }

    pub async fn send_message_for_service(
        &mut self,
        mut req: LspMessageInfo,
        resp_tx: oneshot::Sender<Option<LspMessage>>,
    ) -> Result<()> {
        let resp_tx = match &mut req.message.payload {
            JsonRpcMessage::Request(req) => {
                // if request id was not provided for interceptor,
                // generate one
                if req.id.is_null() {
                    let new_id = self.new_request_last_id + 1;
                    req.id = format!("{}{}", OUR_REQ_ID_PREFIX, new_id).into();
                    self.new_request_last_id = new_id;
                }
                self.pending_interceptor_calls
                    .insert(req.id.to_string(), resp_tx);
                None
            }
            _ => Some(resp_tx),
        };
        let output = match req.to_real_server {
            true => &mut self.real_server_stdin,
            false => &mut self.our_stdout,
        };
        Self::send_message(output, &req.message).await?;
        if let Some(resp_tx) = resp_tx {
            resp_tx
                .send(None)
                .expect("tried to send response back to caller");
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            let should_exit = tokio::select! {
                _ = self.our_stdin.fill_buf() => {
                    self.handle_message(true).await?
                },
                _ = self.real_server_stdout.fill_buf() => {
                    self.handle_message(false).await?
                }
                req = self.service_rx.recv() => {
                    let (req, resp_tx) = req.unwrap();
                    self.send_message_for_service(req, resp_tx).await?;
                    false
                }
            };
            if should_exit {
                break;
            }
        }
        debug!("passthrough is returning");
        // Drop service to drop underlying mpsc channel,
        // so interceptor is notified to exit
        self.interceptor_service = None;
        Ok(())
    }
}
