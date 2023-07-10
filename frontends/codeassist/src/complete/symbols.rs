use anyhow::{anyhow, Result};
use lsp_types::{
    request::SemanticTokensFullRequest, Position, Range, SemanticTokens, SemanticTokensParams,
    TextDocumentIdentifier,
};
use tower::Service;

use crate::{lsp::LspMessage, service::LspMessageInfo};

use super::CodeCompleteTask;

pub(super) struct DescribedPosition {
    pub position: Position,
    pub description: String,
}

impl CodeCompleteTask {
    pub(super) async fn get_relevant_symbol_positions(&mut self) -> Result<Vec<DescribedPosition>> {
        let symbols_response = self
            .passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<SemanticTokensFullRequest>(SemanticTokensParams {
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    text_document: TextDocumentIdentifier::new(self.code_location.uri.clone()),
                })
                .unwrap(),
                true,
            ))
            .await?
            .expect("symbols request should have response");
        let symbols = symbols_response.get_result::<SemanticTokens>()?;

        let mut result = Vec::with_capacity(symbols.data.len());
        let mut current_pos = Position::default();
        let content_manager = self.content_manager.lock().await;
        for symbol in symbols.data {
            if symbol.delta_line != 0 {
                current_pos.line += symbol.delta_line;
                current_pos.character = 0;
            }
            current_pos.character += symbol.delta_start;

            if self.code_location.range.start <= current_pos
                && self.code_location.range.end > current_pos
            {
                let range = Range::new(
                    current_pos.clone(),
                    Position::new(current_pos.line, current_pos.character + symbol.length),
                );

                result.push(DescribedPosition {
                    position: current_pos.clone(),
                    description: content_manager
                        .get_snippet(&self.code_location.uri, &range)
                        .ok_or(anyhow!("failed to get snippet for token"))?,
                });
            }
        }
        Ok(result)
    }

    pub(super) async fn get_relevant_symbol_positions_without_semantic_tokens(
        &mut self,
    ) -> Result<Vec<DescribedPosition>> {
        let mut result = Vec::new();
        let content_manager = self.content_manager.lock().await;
        let snippet = content_manager
            .get_snippet(&self.code_location.uri, &self.code_location.range)
            .ok_or(anyhow!("failed to get snippet for token"))?;
        let mut current_position = self.code_location.range.start.clone();
        let mut current_symbol_position: Option<DescribedPosition> = None;
        for ch in snippet.chars() {
            if ch.is_whitespace() || !ch.is_alphanumeric() {
                if let Some(position) = current_symbol_position.take() {
                    result.push(position);
                }
                if ch == '\n' {
                    current_position.line += 1;
                    current_position.character = 0;
                    continue;
                }
            } else {
                if let Some(position) = current_symbol_position.as_mut() {
                    position.description.push(ch);
                } else {
                    current_symbol_position = Some(DescribedPosition {
                        position: current_position.clone(),
                        description: ch.to_string(),
                    });
                }
            }
            current_position.character += 1;
        }
        if let Some(position) = current_symbol_position {
            result.push(position);
        }
        Ok(result)
    }
}
