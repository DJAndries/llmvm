use std::collections::HashMap;

use anyhow::Result;
use futures::{stream::select_all, StreamExt};
use lsp_types::{
    request::ApplyWorkspaceEdit, ApplyWorkspaceEditParams, Position, Range, TextEdit, WorkspaceEdit,
};
use tower::Service;

use super::{CodeCompleteTask, CompletedSnippet, CompletingSnippet};
use crate::{lsp::LspMessage, service::LspMessageInfo};

const CODE_WRAP_MD_TOKEN: &str = "``";

#[derive(Default)]
struct SnippetOffsetInfo {
    position: Position,
    is_skipping: bool,
}

impl CodeCompleteTask {
    fn wrap_snippet(snippet: String, header: String) -> String {
        let top_line = format!(">>=={:=<40}=#", header);
        let bottom_line = format!("<<=={:=<40}=#", "");
        format!("{}\n{}\n{}\n", top_line, snippet, bottom_line)
    }

    async fn apply_edit(&mut self, range: Range, snippet_text: String) -> Result<()> {
        let text_edit = TextEdit::new(range, snippet_text);
        let changes = HashMap::from([(self.code_location.uri.clone(), vec![text_edit])]);
        self.passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<ApplyWorkspaceEdit>(ApplyWorkspaceEditParams {
                    label: None,
                    edit: WorkspaceEdit::new(changes),
                })
                .unwrap(),
                false,
            ))
            .await?;
        Ok(())
    }

    pub(super) async fn apply_completed_snippet_edits(
        &mut self,
        completed_snippets: Vec<CompletedSnippet>,
    ) -> Result<()> {
        let insert_in_place = self.config.prefer_insert_in_place && completed_snippets.len() == 1;
        let mut snippets_text = String::new();
        for completed_snippet in completed_snippets {
            if let Some(thread_id) = completed_snippet.response.thread_id {
                *self.thread_id.clone().lock().await = Some(thread_id);
            }
            let snippet_text = completed_snippet
                .response
                .response
                .split("\n")
                .filter(|line| !line.starts_with(CODE_WRAP_MD_TOKEN))
                .collect::<Vec<&str>>()
                .join("\n");
            snippets_text += match insert_in_place {
                false => Self::wrap_snippet(snippet_text, completed_snippet.preset),
                true => snippet_text,
            }
            .as_str();
        }
        let (range, snippets_text) = match insert_in_place {
            false => {
                snippets_text.push('\n');
                let next_line_pos = Position::new(self.code_location.range.end.line, 0);
                (
                    Range::new(next_line_pos.clone(), next_line_pos),
                    snippets_text,
                )
            }
            true => (self.code_location.range.clone(), snippets_text),
        };
        self.apply_edit(range, snippets_text).await?;
        Ok(())
    }

    fn update_offset_and_filter_text<'a>(
        &self,
        offset: &mut SnippetOffsetInfo,
        text: &'a str,
    ) -> &'a str {
        let mut filtered_start_index = 0;
        for (i, ch) in text.chars().enumerate() {
            if ch == '\n' {
                if offset.is_skipping {
                    filtered_start_index += 1;
                    offset.is_skipping = false;
                } else {
                    offset.position.line += 1;
                }
                offset.position.character = 0;
            } else if !offset.is_skipping {
                if offset.position.character == 0 && &text[0..(i + 1)] == CODE_WRAP_MD_TOKEN {
                    offset.is_skipping = true;

                    filtered_start_index = i + 1;
                    offset.position.character = 0;
                } else {
                    offset.position.character += 1;
                }
            } else {
                filtered_start_index += 1;
            }
        }
        &text[filtered_start_index..text.len()]
    }

    pub(super) async fn apply_completing_snippet_edits(
        &mut self,
        completing_snippets: Vec<CompletingSnippet>,
    ) -> Result<()> {
        let insert_in_place = self.config.prefer_insert_in_place && completing_snippets.len() == 1;
        let start_position = match insert_in_place {
            true => {
                self.apply_edit(self.code_location.range.clone(), "".to_string())
                    .await?;
                self.code_location.range.start.clone()
            }
            false => {
                let mut position = Position::new(self.code_location.range.end.line, 0);
                let snippet_wrappers = completing_snippets
                    .iter()
                    .map(|s| Self::wrap_snippet(String::new(), s.preset.clone()))
                    .collect::<Vec<_>>()
                    .join("");
                self.apply_edit(
                    Range::new(position.clone(), position.clone()),
                    snippet_wrappers,
                )
                .await?;
                // skip the initial snippet header
                position.line += 1;
                position
            }
        };
        let mut snippet_offsets: Vec<_> = completing_snippets
            .iter()
            .map(|_| SnippetOffsetInfo::default())
            .collect();
        let mut streams = select_all(completing_snippets.into_iter().map(|cs| cs.stream));
        while let Some(response) = streams.next().await {
            let before_line_offset_sum =
                snippet_offsets[0..response.id]
                    .iter()
                    .fold(0, |mut acc, offset| {
                        acc += offset.position.line;
                        if !insert_in_place {
                            // skip another snippet header
                            acc += 3;
                        }
                        acc
                    });
            let offset = snippet_offsets
                .as_mut_slice()
                .iter_mut()
                .enumerate()
                .find(|(id, _)| id == &response.id)
                .map(|(_, offset)| offset)
                .unwrap();
            let total_line_offset = before_line_offset_sum + offset.position.line;
            let character_pos = match total_line_offset == 0 {
                true => start_position.character + offset.position.character,
                false => offset.position.character,
            };
            let real_position =
                Position::new(start_position.line + total_line_offset, character_pos);
            let response = response.response?;

            if let Some(thread_id) = response.thread_id {
                *self.thread_id.lock().await = Some(thread_id);
            }

            let text = response.response;
            let filtered_text = self.update_offset_and_filter_text(offset, &text);
            self.apply_edit(
                Range::new(real_position.clone(), real_position.clone()),
                filtered_text.to_string(),
            )
            .await?;
        }

        Ok(())
    }
}
