use std::{collections::HashMap, pin::Pin};

use anyhow::Result;
use llmvm_protocol::jsonrpc::JsonRpcMessage;
use serde_json::Value;
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, Stdin,
        Stdout,
    },
    process::{ChildStdin, ChildStdout},
    sync::{mpsc, oneshot},
};
use tracing::{debug, error};

use crate::{
    interceptor::LspInterceptor,
    lsp::{LspMessage, CONTENT_LENGTH_HEADER},
    service::{LspMessageInfo, LspMessageService, LspMessageTrx},
};

const OUR_REQ_ID_PREFIX: &str = "llmvm/";

pub struct LspStdioPassthrough {
    our_stdin: Pin<Box<dyn AsyncBufRead>>,
    our_stdout: Pin<Box<dyn AsyncWrite>>,
    real_server_stdin: Pin<Box<dyn AsyncWrite>>,
    real_server_stdout: Pin<Box<dyn AsyncBufRead>>,

    service_rx: mpsc::UnboundedReceiver<LspMessageTrx>,
    service_tx: Option<mpsc::UnboundedSender<LspMessageTrx>>,

    interceptor: Option<LspInterceptor>,

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
            interceptor: None,
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

    pub fn set_adapter_service(&mut self, service: LspMessageService) {
        self.interceptor = Some(LspInterceptor::new(service));
    }

    async fn send_message(&mut self, message_info: &LspMessageInfo) -> Result<()> {
        let output = match message_info.to_real_server {
            true => &mut self.real_server_stdin,
            false => &mut self.our_stdout,
        };
        let payload_vec = match serde_json::to_vec(&message_info.message.payload) {
            Ok(p) => p,
            // TODO: add tracing for error
            // Err(_) => return Ok(should_exit),
            Err(_) => return Ok(()),
        };
        for (key, value) in &message_info.message.headers {
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

    /// Reads headers into the out params, returns false if the input stream is finished
    async fn read_headers(
        input: &mut Pin<Box<dyn AsyncBufRead>>,
        headers: &mut HashMap<String, String>,
        content_length: &mut Option<usize>,
    ) -> Result<bool> {
        let mut header_line = String::new();
        loop {
            if input.read_line(&mut header_line).await? == 0 {
                return Ok(false);
            }
            let header_trimmed = header_line.trim();
            if header_trimmed.is_empty() {
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
                *content_length = match header_value.parse::<usize>() {
                    Ok(length) => Some(length),
                    Err(_) => break,
                };
            }
            headers.insert(header_key.to_string(), header_value.to_string());

            header_line.clear();
        }
        Ok(true)
    }

    // Reads message payload for given content length, returns None if
    // message could not be read and should be skipped.
    async fn read_message_payload(
        input: &mut Pin<Box<dyn AsyncBufRead>>,
        content_length: usize,
    ) -> Result<Option<JsonRpcMessage>> {
        let mut payload_vec = vec![0u8; content_length];
        input.read_exact(&mut payload_vec).await?;

        Ok(match serde_json::from_slice::<Value>(&payload_vec) {
            Ok(payload) => match JsonRpcMessage::try_from(payload) {
                Ok(payload) => Some(payload),
                Err(e) => {
                    error!("failed to parse lsp json rpc message: {e}");
                    None
                }
            },
            // TODO: add tracing for error
            Err(e) => {
                error!("failed to parse lsp json: {e}");
                None
            }
        })
    }

    // Reads headers and message, and processes message. Returns true if the LSP
    // is exiting.
    async fn handle_message(&mut self, to_real_server: bool) -> Result<bool> {
        let input = match to_real_server {
            true => &mut self.our_stdin,
            false => &mut self.real_server_stdout,
        };
        let mut headers = HashMap::new();
        let mut content_length = None;

        if !Self::read_headers(input, &mut headers, &mut content_length).await?
            || content_length.is_none()
        {
            return Ok(false);
        }

        let payload = Self::read_message_payload(input, content_length.unwrap()).await?;

        match payload {
            Some(payload) => {
                let interceptor_result = self
                    .interceptor
                    .as_mut()
                    .unwrap()
                    .maybe_intercept(LspMessageInfo::new(
                        LspMessage { headers, payload },
                        to_real_server,
                    ))
                    .await?;

                if let Some(message_info) = interceptor_result.forwarded_message_info {
                    // Send forwarded message evaluated from interceptor
                    self.send_message(&message_info).await?;
                }
                Ok(interceptor_result.should_exit)
            }
            None => {
                // Could not read message payload, skip this one.
                Ok(false)
            }
        }
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
                self.interceptor
                    .as_mut()
                    .unwrap()
                    .add_pending_interceptor_call(&req, resp_tx);
                None
            }
            _ => Some(resp_tx),
        };
        self.send_message(&req).await?;
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
        // Drop interceptor (and by proxy, the adapter service) to drop
        // underlying mpsc channel, so adapter is notified to exit
        self.interceptor = None;
        Ok(())
    }
}
