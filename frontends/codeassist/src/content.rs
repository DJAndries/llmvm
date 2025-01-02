use anyhow::{anyhow, Result};
use llmvm_protocol::{
    jsonrpc::JsonRpcMessage,
    service::{CoreRequest, CoreResponse},
    BoxedService, SessionPromptParameter, StoreSessionPromptParameterRequest,
};
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    },
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Range, Url,
};
use serde::Serialize;
use std::{cmp::min, collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, Mutex},
    time::sleep,
};
use tokio_stream::{wrappers::LinesStream, StreamExt};
use tracing::{error, trace};

use crate::{lsp::LspMessage, session::ALL_SESSION_TAGS};

const UNKNOWN_LANG_ID: &str = "unknown";

struct DocumentInfo {
    lines: Vec<String>,
    lang_id: String,
    file_context_update_notifier: Option<mpsc::UnboundedSender<()>>,
}

#[derive(Serialize)]
struct FileContext {
    content: String,
    file_path: String,
}

pub struct ContentManager {
    content_map: HashMap<Url, DocumentInfo>,
    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
    session_id: String,
}

impl ContentManager {
    pub fn new(
        llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
        session_id: String,
    ) -> Self {
        Self {
            content_map: HashMap::new(),
            llmvm_core_service,
            session_id,
        }
    }

    pub async fn maybe_load_file(&mut self, uri: &Url) -> Result<()> {
        if !self.content_map.contains_key(uri) {
            let reader = BufReader::new(File::open(uri.path()).await?);
            let lines = LinesStream::new(reader.lines())
                .collect::<Result<Vec<String>, std::io::Error>>()
                .await?;
            self.set_document(uri, lines, None)?;
        }
        Ok(())
    }

    pub fn get_line(&self, uri: &Url, line: usize) -> Option<&str> {
        self.content_map
            .get(uri)
            .and_then(|doc_info| doc_info.lines.get(line))
            .map(|v| v.as_str())
    }

    pub fn get_snippet(&self, uri: &Url, range: &Range) -> Option<String> {
        self.content_map
            .get(uri)
            .filter(|doc_info| !doc_info.lines.is_empty())
            .map(|doc_info| {
                let start_line = min(range.start.line as usize, doc_info.lines.len() - 1);
                let end_line = min(range.end.line as usize, doc_info.lines.len() - 1);
                let range_lines = doc_info.lines[start_line..=end_line]
                    .iter()
                    .zip(start_line..=end_line)
                    .map(|(line, line_index)| {
                        let start_char = match line_index == start_line {
                            true => min(
                                range.start.character as usize,
                                if line.is_empty() { 0 } else { line.len() - 1 },
                            ),
                            false => 0,
                        };
                        let end_char = match line_index == end_line {
                            true => min(range.end.character as usize, line.len()),
                            false => line.len(),
                        };
                        &line[start_char..end_char]
                    })
                    .collect::<Vec<&str>>();
                range_lines.join("\n")
            })
    }

    pub fn get_lang_id(&self, uri: &Url) -> String {
        self.content_map
            .get(uri)
            .map(|v| v.lang_id.clone())
            .unwrap_or_else(|| UNKNOWN_LANG_ID.to_string())
    }

    pub fn line_count(&self, uri: &Url) -> usize {
        self.content_map
            .get(uri)
            .map(|doc_info| doc_info.lines.len())
            .unwrap_or_default()
    }

    fn split_text(text: &str) -> Vec<String> {
        text.split("\n").map(|v| v.to_string()).collect()
    }

    fn apply_patch(&mut self, uri: &Url, range: &Range, text: &str) -> Result<()> {
        let stored_lines = self
            .content_map
            .get_mut(uri)
            .map(|doc_info| &mut doc_info.lines);
        if let Some(stored_lines) = stored_lines {
            let mut new_lines: Vec<String> = Self::split_text(&text);

            if range.start.character > 0 {
                if let Some(stored_line) = stored_lines.get(range.start.line as usize) {
                    new_lines[0] = format!(
                        "{}{}",
                        &stored_line[..min(range.start.character as usize, stored_line.len())],
                        new_lines[0]
                    );
                }
            }

            if let Some(stored_line) = stored_lines.get(range.end.line as usize) {
                if (range.end.character as usize) < stored_line.len() {
                    let last_new_line_index = new_lines.len() - 1;
                    new_lines[last_new_line_index] = format!(
                        "{}{}",
                        new_lines[last_new_line_index],
                        &stored_line[range.end.character as usize..]
                    );
                }
            }

            stored_lines.splice(
                range.start.line as usize..=range.end.line as usize,
                new_lines,
            );
            trace!(
                uri = uri.as_str(),
                "patch applied, updated doc: {:#?}",
                stored_lines
            );

            self.notify_file_context_update(uri);
        }

        Ok(())
    }

    fn set_document(
        &mut self,
        uri: &Url,
        lines: Vec<String>,
        lang_id: Option<String>,
    ) -> Result<()> {
        trace!(
            uri = uri.as_str(),
            "setting document, new doc: {:#?}",
            lines
        );
        match self.content_map.get_mut(uri) {
            Some(doc_info) => {
                doc_info.lines = lines;
                if let Some(lang_id) = lang_id {
                    doc_info.lang_id = lang_id;
                }
                self.notify_file_context_update(uri);
            }
            None => {
                self.content_map.insert(
                    uri.clone(),
                    DocumentInfo {
                        lines,
                        lang_id: lang_id.unwrap_or_else(|| UNKNOWN_LANG_ID.to_string()),
                        file_context_update_notifier: None,
                    },
                );
            }
        };
        Ok(())
    }

    pub fn is_file_context_enabled(&self, uri: &Url) -> bool {
        self.content_map.get(uri).map_or(false, |doc_info| {
            doc_info.file_context_update_notifier.is_some()
        })
    }

    pub fn is_any_file_context_enabled(&self) -> bool {
        self.content_map
            .values()
            .any(|doc_info| doc_info.file_context_update_notifier.is_some())
    }

    pub fn toggle_file_context(&mut self, content_manager_arc: &Arc<Mutex<Self>>, uri: &Url) {
        if let Some(doc_info) = self.content_map.get_mut(uri) {
            if doc_info.file_context_update_notifier.is_some() {
                doc_info.file_context_update_notifier = None;
                return;
            }
            let (tx, mut rx) = mpsc::unbounded_channel::<()>();
            doc_info.file_context_update_notifier = Some(tx);

            let uri_clone = uri.clone();
            let content_manager = content_manager_arc.clone();

            tokio::spawn(async move {
                let mut storage_pending = false;
                loop {
                    tokio::select! {
                        _ = sleep(Duration::from_secs(5)) => {
                            if !storage_pending {
                                continue;
                            }
                            storage_pending = false;
                            if let Err(e) = content_manager
                                .lock()
                                .await
                                .set_file_context_session_parameter(&uri_clone, true)
                                .await {
                                error!("failed to store session prompt parameter: {e}");
                            }
                        }
                        result = rx.recv() => {
                            match result {
                                None => {
                                    if let Err(e) = content_manager
                                        .lock()
                                        .await
                                        .set_file_context_session_parameter(&uri_clone, false)
                                        .await {
                                        error!("failed to clear session prompt parameter: {e}");
                                    }
                                    return;
                                },
                                Some(()) => storage_pending = true
                            };
                        }
                    }
                }
            });

            self.notify_file_context_update(uri);
        }
    }

    pub fn disable_all_file_contexts(&mut self) {
        for doc_info in self.content_map.values_mut() {
            doc_info.file_context_update_notifier = None;
        }
    }

    async fn set_file_context_session_parameter(&self, uri: &Url, enabled: bool) -> Result<()> {
        let file_path = uri.path().to_string();
        let param_key = format!("codeassist_file_content.{}", file_path.replace(".", "_"));
        let parameter = match enabled {
            true => match self.content_map.get(uri) {
                Some(doc_info) => {
                    let content = doc_info
                        .lines
                        .iter()
                        .enumerate()
                        .map(|(i, v)| format!("{}: {}", i + 1, v))
                        .collect::<Vec<String>>()
                        .join("\n");

                    Some(SessionPromptParameter {
                        persistent: true,
                        value: serde_json::to_value(FileContext {
                            content: content.clone(),
                            file_path: file_path.clone(),
                        })?,
                    })
                }
                None => None,
            },
            false => None,
        };

        for tag in ALL_SESSION_TAGS {
            self.llmvm_core_service
                .lock()
                .await
                .call(CoreRequest::StoreSessionPromptParameter(
                    StoreSessionPromptParameterRequest {
                        key: param_key.clone(),
                        session_id: self.session_id.clone(),
                        session_tag: tag.to_string(),
                        parameter: parameter.clone(),
                    },
                ))
                .await
                .map_err(|e| anyhow!(e))?;
        }
        Ok(())
    }

    fn notify_file_context_update(&self, uri: &Url) {
        if let Some(doc_info) = self.content_map.get(uri) {
            if let Some(sender) = &doc_info.file_context_update_notifier {
                let _ = sender.send(());
            }
        }
    }

    pub fn handle_doc_sync_notification(&mut self, message: &LspMessage) -> Result<()> {
        match &message.payload {
            JsonRpcMessage::Notification(notification) => match notification.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    let params = message.get_params::<DidOpenTextDocumentParams>()?;
                    let lines = Self::split_text(&params.text_document.text);
                    self.set_document(
                        &params.text_document.uri,
                        lines,
                        Some(params.text_document.language_id),
                    )?;
                }
                DidChangeTextDocument::METHOD => {
                    let params = message.get_params::<DidChangeTextDocumentParams>()?;

                    for change in params.content_changes {
                        match change.range {
                            Some(range) => {
                                self.apply_patch(&params.text_document.uri, &range, &change.text)?
                            }
                            None => self.set_document(
                                &params.text_document.uri,
                                Self::split_text(&change.text),
                                None,
                            )?,
                        }
                    }
                }
                DidCloseTextDocument::METHOD => {
                    let params = message.get_params::<DidCloseTextDocumentParams>()?;
                    self.content_map.remove(&params.text_document.uri);
                    trace!(uri = params.text_document.uri.as_str(), "document closed");
                }
                _ => (),
            },
            _ => (),
        }
        Ok(())
    }
}
