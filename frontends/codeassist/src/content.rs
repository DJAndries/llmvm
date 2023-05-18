use anyhow::Result;
use llmvm_protocol::jsonrpc::JsonRpcMessage;
use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    },
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Range, Url,
};
use std::{cmp::min, collections::HashMap};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
};
use tokio_stream::{wrappers::LinesStream, StreamExt};
use tracing::trace;

use crate::lsp::LspMessage;

const UNKNOWN_LANG_ID: &str = "unknown";

#[derive(Debug)]
pub struct SnippetInfo {
    pub description: String,
    pub snippet: String,
}

struct DocumentInfo {
    lines: Vec<String>,
    lang_id: String,
}

#[derive(Default)]
pub struct ContentManager {
    content_map: HashMap<Url, DocumentInfo>,
}

impl ContentManager {
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
            }
            None => {
                self.content_map.insert(
                    uri.clone(),
                    DocumentInfo {
                        lines,
                        lang_id: lang_id.unwrap_or_else(|| UNKNOWN_LANG_ID.to_string()),
                    },
                );
            }
        };
        Ok(())
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
