use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use anyhow::{anyhow, Result};
use lsp_types::{
    request::{
        FoldingRangeRequest, GotoTypeDefinition, GotoTypeDefinitionParams,
        GotoTypeDefinitionResponse,
    },
    FoldingRange, FoldingRangeParams, Position, Range, TextDocumentIdentifier,
    TextDocumentPositionParams, Url,
};
use tower::Service;

use crate::{lsp::LspMessage, service::LspMessageInfo};

use super::{symbols::DescribedPosition, CodeCompleteTask, ContextSnippet, HashableLocation};

#[derive(Debug, PartialEq, Eq, Hash)]
pub(super) struct SimpleFoldingRange {
    start_line: u32,
    end_line: u32,
}

impl From<FoldingRange> for SimpleFoldingRange {
    fn from(value: FoldingRange) -> Self {
        Self {
            start_line: value.start_line,
            end_line: value.end_line,
        }
    }
}

impl CodeCompleteTask {
    pub(super) async fn get_type_definition_locations(
        &mut self,
        symbol_positions: Vec<DescribedPosition>,
    ) -> Result<HashMap<HashableLocation, Vec<String>>> {
        let mut result: HashMap<HashableLocation, Vec<String>> = HashMap::new();
        for position in symbol_positions {
            let typedef_response = self
                .passthrough_service
                .call(LspMessageInfo::new(
                    LspMessage::new_request::<GotoTypeDefinition>(GotoTypeDefinitionParams {
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        text_document_position_params: TextDocumentPositionParams::new(
                            TextDocumentIdentifier::new(self.code_location.uri.clone()),
                            position.position,
                        ),
                    })
                    .unwrap(),
                    true,
                ))
                .await?
                .expect("type definition request should have response");
            let typedef_result = typedef_response.get_result::<GotoTypeDefinitionResponse>()?;
            let locations = match typedef_result {
                GotoTypeDefinitionResponse::Scalar(location) => vec![location],
                GotoTypeDefinitionResponse::Array(locations) => locations,
                _ => Vec::new(),
            };
            for location in locations {
                if let Some(root_uri) = &self.root_uri {
                    if !location.uri.as_str().starts_with(root_uri.as_str()) {
                        continue;
                    }
                }
                result
                    .entry(HashableLocation(location))
                    .or_default()
                    .push(position.description.clone());
            }
        }
        Ok(result)
    }

    pub(super) async fn get_folding_ranges(
        &mut self,
        typedef_locations: &HashMap<HashableLocation, Vec<String>>,
    ) -> Result<HashMap<Url, HashSet<SimpleFoldingRange>>> {
        let mut result = HashMap::new();
        for location in typedef_locations.keys() {
            if result.contains_key(&location.0.uri) {
                continue;
            }

            let folding_range_response = self
                .passthrough_service
                .call(LspMessageInfo::new(
                    LspMessage::new_request::<FoldingRangeRequest>(FoldingRangeParams {
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        text_document: TextDocumentIdentifier::new(location.0.uri.clone()),
                    })
                    .unwrap(),
                    true,
                ))
                .await?
                .expect("folding range request should have response");

            if let Some(ranges) =
                folding_range_response.get_result::<Option<Vec<FoldingRange>>>()?
            {
                result.insert(
                    location.0.uri.clone(),
                    ranges.into_iter().map(|f| f.into()).collect(),
                );
            }
        }
        Ok(result)
    }

    pub(super) async fn get_typedef_context_snippets(
        &mut self,
        typedef_locations: &HashMap<HashableLocation, Vec<String>>,
        folding_ranges: &HashMap<Url, HashSet<SimpleFoldingRange>>,
    ) -> Result<Vec<ContextSnippet>> {
        let mut result = Vec::new();
        let mut content_manager = self.content_manager.lock().await;
        for (location, descriptions) in typedef_locations {
            content_manager.maybe_load_file(&location.0.uri).await?;
            let mut snippet_range = Range::new(
                Position::new(location.0.range.start.line as u32, 0),
                Position::new(location.0.range.end.line as u32, u32::MAX),
            );
            for line in
                snippet_range.start.line as usize..content_manager.line_count(&location.0.uri)
            {
                let mut new_end_line = None;
                if let Some(folding_ranges) = folding_ranges.get(&location.0.uri) {
                    if let Some(folding_range) = folding_ranges
                        .iter()
                        .find(|v| v.start_line as usize == line)
                    {
                        new_end_line = Some(folding_range.end_line as usize);
                    }
                }
                if new_end_line.is_none() {
                    if let Some(line_text) = content_manager.get_line(&location.0.uri, line) {
                        if line_text.trim().is_empty() {
                            new_end_line = Some(if line > 0 { line - 1 } else { 0 });
                        }
                    }
                }
                if let Some(new_end_line) = new_end_line {
                    if new_end_line > snippet_range.end.line as usize {
                        snippet_range.end.line = new_end_line as u32;
                    }
                    break;
                }
            }
            let descriptions = Some(
                descriptions
                    .iter()
                    .map(|d| format!("\"{d}\""))
                    .collect::<Vec<String>>()
                    .join(", "),
            );
            result.push(ContextSnippet {
                descriptions,
                snippet: content_manager
                    .get_snippet(&location.0.uri, &snippet_range)
                    .ok_or(anyhow!("failed to get context snippet from fetcher"))?,
            });
        }
        Ok(result)
    }
}
