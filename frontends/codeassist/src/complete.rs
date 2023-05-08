use std::{
    collections::{HashMap, HashSet},
    error::Error,
    hash::{Hash, Hasher},
    sync::Arc,
};

use anyhow::{anyhow, bail, Result};
use futures::future::join_all;
use llmvm_protocol::{
    stdio::{CoreRequest, CoreResponse, StdioClient},
    GenerationParameters, GenerationRequest,
};
use lsp_types::{
    notification::{Progress, ShowMessage},
    request::{
        ApplyWorkspaceEdit, DocumentSymbolRequest, FoldingRangeRequest, GotoTypeDefinition,
        GotoTypeDefinitionParams, GotoTypeDefinitionResponse, SelectionRangeRequest,
        SemanticTokensFullRequest, SemanticTokensRangeRequest, WorkDoneProgressCreate,
    },
    ApplyWorkspaceEditParams, DocumentSymbolParams, FoldingRange, FoldingRangeParams, Location,
    MessageType, PartialResultParams, Position, ProgressParams, ProgressParamsValue, ProgressToken,
    Range, SelectionRangeParams, SemanticTokens, SemanticTokensDeltaParams, SemanticTokensParams,
    SemanticTokensRangeParams, ServerCapabilities, ShowMessageParams, TextDocumentIdentifier,
    TextDocumentPositionParams, TextEdit, Url, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressParams,
    WorkDoneProgressReport, WorkspaceEdit,
};
use serde::Serialize;
use tokio::{sync::Mutex, task::JoinError};
use tower::{buffer::Buffer, timeout::Timeout, util::Ready, Service, ServiceExt};
use tracing::debug;

use crate::{
    content::ContentManager,
    lsp::LspMessage,
    service::{LspMessageInfo, LspMessageService},
    CodeAssistConfig,
};

const PROGRESS_TOKEN_PREFIX: &str = "llmvm/complete/";
const PRESET_LIST_PREFIX: &str = "ccpr=";

#[derive(Debug, PartialEq, Eq)]
pub struct HashableLocation(pub Location);

impl Hash for HashableLocation {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.uri.hash(state);
        self.0.range.start.line.hash(state);
        self.0.range.start.character.hash(state);
        self.0.range.end.line.hash(state);
        self.0.range.end.character.hash(state);
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SimpleFoldingRange {
    start_line: u32,
    end_line: u32,
}

struct DescribedPosition {
    position: Position,
    description: String,
}

#[derive(Debug, Clone, Serialize)]
struct ContextSnippet {
    descriptions: Option<String>,
    snippet: String,
}

#[derive(Clone, Serialize)]
struct PromptParameters {
    lang: String,
    typedef_context_snippets: Vec<ContextSnippet>,
    random_context_snippets: Vec<ContextSnippet>,
    main_snippet: String,
}

#[derive(Debug)]
struct CompletedSnippet {
    preset: String,
    snippet: String,
}

impl From<FoldingRange> for SimpleFoldingRange {
    fn from(value: FoldingRange) -> Self {
        Self {
            start_line: value.start_line,
            end_line: value.end_line,
        }
    }
}

pub struct CodeCompleteTask {
    config: Arc<CodeAssistConfig>,

    llmvm_core_service: Timeout<StdioClient<CoreRequest, CoreResponse>>,
    passthrough_service: LspMessageService,
    root_uri: Option<Url>,
    supports_semantic_tokens: bool,
    supports_folding_ranges: bool,
    supports_type_definitions: bool,

    content_manager: Arc<Mutex<ContentManager>>,
    code_location: Location,

    task_id: usize,
    random_context_locations: Vec<Location>,
}

impl CodeCompleteTask {
    // TODO: add optional custom prompt
    pub fn new(
        config: Arc<CodeAssistConfig>,
        llmvm_core_service: Timeout<StdioClient<CoreRequest, CoreResponse>>,
        passthrough_service: LspMessageService,
        server_capabilities: Option<&ServerCapabilities>,
        root_uri: Option<Url>,
        content_manager: Arc<Mutex<ContentManager>>,
        code_location: Location,
        task_id: usize,
        random_context_locations: Vec<Location>,
    ) -> Self {
        let supports_semantic_tokens = server_capabilities
            .map(|c| c.semantic_tokens_provider.is_some())
            .unwrap_or_default();
        let supports_folding_ranges = server_capabilities
            .map(|c| c.folding_range_provider.is_some())
            .unwrap_or_default();
        let supports_type_definitions = server_capabilities
            .map(|c| c.type_definition_provider.is_some())
            .unwrap_or_default();
        Self {
            config,
            llmvm_core_service,
            passthrough_service,
            root_uri,
            supports_semantic_tokens,
            supports_folding_ranges,
            supports_type_definitions,
            content_manager,
            code_location,
            task_id,
            random_context_locations,
        }
    }
    fn extract_preset_list(&self, main_snippet: &mut String) -> Vec<String> {
        let mut is_present = false;
        let preset_list = main_snippet
            .find(PRESET_LIST_PREFIX)
            .map(|start_index| {
                is_present = true;
                let start_index = start_index + PRESET_LIST_PREFIX.len();
                let end_index = main_snippet[start_index..]
                    .find(char::is_whitespace)
                    .map(|wp_index| start_index + wp_index)
                    .unwrap_or(main_snippet.len());

                let substr = &main_snippet[start_index..end_index];
                substr.split(',').map(|s| s.trim().to_string()).collect()
            })
            .unwrap_or_else(|| vec![self.config.default_preset.clone()]);
        // remove preset list from snippet so it doesn't feed into llm
        if is_present {
            *main_snippet = main_snippet
                .lines()
                .filter(|l| !l.contains(PRESET_LIST_PREFIX))
                .collect::<Vec<_>>()
                .join("\n");
        }
        preset_list
    }

    async fn get_relevant_symbol_positions(&mut self) -> Result<Vec<DescribedPosition>> {
        // TODO: guess the relevant symbols for LSP servers that don't support semantic tokens
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

    async fn get_relevant_symbol_positions_without_semantic_tokens(
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

    async fn get_type_definition_locations(
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

    async fn get_folding_ranges(
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

    async fn get_typedef_context_snippets(
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

    async fn get_random_context_snippets(&self) -> Result<Vec<ContextSnippet>> {
        let mut result = Vec::new();
        let content_manager = self.content_manager.lock().await;
        for location in &self.random_context_locations {
            result.push(ContextSnippet {
                descriptions: None,
                snippet: content_manager
                    .get_snippet(&location.uri, &location.range)
                    .ok_or(anyhow!("failed to get context snippet from fetcher"))?,
            });
        }
        Ok(result)
    }

    async fn send_generation_request(
        mut core_service: Timeout<StdioClient<CoreRequest, CoreResponse>>,
        preset: String,
        prompt_params: PromptParameters,
    ) -> Result<CompletedSnippet> {
        let response = core_service
            .call(CoreRequest::Generation(GenerationRequest {
                parameters: Some(GenerationParameters {
                    prompt_parameters: Some(serde_json::to_value(prompt_params)?),
                    ..Default::default()
                }),
                existing_thread_id: None,
                save_thread: false,
                preset_id: Some(preset.clone()),
            }))
            .await
            .map_err(|e| anyhow!(e))??;
        match response {
            CoreResponse::Generation(response) => Ok(CompletedSnippet {
                preset,
                snippet: response.response,
            }),
            _ => bail!("unexpected response from llmvm"),
        }
    }

    async fn create_progress_token(&mut self) -> Result<ProgressToken> {
        let token = ProgressToken::String(format!("{}{}", PROGRESS_TOKEN_PREFIX, self.task_id));
        let result = self
            .passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                    token: token.clone(),
                })?,
                false,
            ))
            .await
            .map_err(|e| anyhow!(e))?;
        result
            .ok_or(anyhow!("missing response for progress token creation"))?
            .get_result()?;
        Ok(token)
    }

    async fn notify_user(&mut self, token: &ProgressToken, is_complete: bool) -> Result<()> {
        let progresses = match is_complete {
            false => vec![
                WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: "Text Generation".to_string(),
                    message: Some("Starting generation".to_string()),
                    cancellable: Some(false),
                    ..Default::default()
                }),
                WorkDoneProgress::Report(WorkDoneProgressReport {
                    message: Some("Generating...".to_string()),
                    cancellable: Some(false),
                    ..Default::default()
                }),
            ],
            true => vec![
                WorkDoneProgress::Report(WorkDoneProgressReport {
                    message: Some("Generation complete".to_string()),
                    cancellable: Some(false),
                    ..Default::default()
                }),
                WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some("Generation complete".to_string()),
                }),
            ],
        };
        for (i, progress) in progresses.into_iter().enumerate() {
            let mut passthrough_service = self.passthrough_service.clone();
            let token = token.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(i as u64)).await;
                passthrough_service
                    .call(LspMessageInfo::new(
                        LspMessage::new_notification::<Progress>(ProgressParams {
                            token,
                            value: ProgressParamsValue::WorkDone(progress),
                        })
                        .unwrap(),
                        false,
                    ))
                    .await
                    .ok();
            });
        }
        Ok(())
    }

    fn wrap_snippet(snippet: String, header: String) -> String {
        let top_line = format!(">>=={:=<40}=#", header);
        let bottom_line = format!("<<=={:=<40}=#", "");
        format!("{}\n{}\n{}\n", top_line, snippet, bottom_line)
    }

    async fn apply_edit(&mut self, completed_snippets: Vec<CompletedSnippet>) -> Result<()> {
        let insert_in_place = self.config.prefer_insert_in_place && completed_snippets.len() == 1;
        let mut snippets_text = completed_snippets
            .into_iter()
            .map(|completed_snippet| {
                let snippet_text = completed_snippet
                    .snippet
                    .split("\n")
                    .filter(|line| !line.starts_with("```"))
                    .collect::<Vec<&str>>()
                    .join("\n");
                match insert_in_place {
                    false => Self::wrap_snippet(snippet_text, completed_snippet.preset),
                    true => snippet_text,
                }
            })
            .collect::<Vec<_>>()
            .join("");
        let mut changes = HashMap::new();
        let text_edit = match insert_in_place {
            false => {
                snippets_text.push('\n');
                let next_line_pos = Position::new(self.code_location.range.end.line + 1, 0);
                TextEdit::new(
                    Range::new(next_line_pos.clone(), next_line_pos),
                    snippets_text,
                )
            }
            true => TextEdit::new(self.code_location.range.clone(), snippets_text),
        };
        changes.insert(self.code_location.uri.clone(), vec![text_edit]);
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

    pub async fn run(mut self) -> Result<()> {
        let progress_token = self.create_progress_token().await?;

        self.content_manager
            .lock()
            .await
            .maybe_load_file(&self.code_location.uri)
            .await?;

        let typedef_context_snippets = if self.supports_type_definitions {
            let symbols = if !self.supports_semantic_tokens {
                debug!("getting symbol positions via semantic tokens");
                self.get_relevant_symbol_positions().await?
            } else {
                debug!("getting symbol positions via whitespace");
                self.get_relevant_symbol_positions_without_semantic_tokens()
                    .await?
            };

            let typedef_locations = self.get_type_definition_locations(symbols).await?;

            let folding_ranges = if self.supports_folding_ranges {
                self.get_folding_ranges(&typedef_locations).await?
            } else {
                Default::default()
            };

            self.get_typedef_context_snippets(&typedef_locations, &folding_ranges)
                .await?
        } else {
            Default::default()
        };

        let random_context_snippets = self.get_random_context_snippets().await?;

        let mut main_snippet = self
            .content_manager
            .lock()
            .await
            .get_snippet(&self.code_location.uri, &self.code_location.range)
            .ok_or(anyhow!("failed to get main snippet from fetcher"))?;

        let presets = self.extract_preset_list(&mut main_snippet);

        debug!("typedef context: {:#?}", typedef_context_snippets);
        debug!("main snippet: {:#?}", main_snippet);
        debug!("presets: {:#?}", presets);

        self.notify_user(&progress_token, false).await?;

        let prompt_params = PromptParameters {
            lang: self
                .content_manager
                .lock()
                .await
                .get_lang_id(&self.code_location.uri),
            typedef_context_snippets,
            random_context_snippets,
            main_snippet,
        };
        let tasks: Vec<_> = presets
            .into_iter()
            .map(|preset| {
                let core_service = self.llmvm_core_service.clone();
                let prompt_params = prompt_params.clone();
                tokio::spawn(async move {
                    Self::send_generation_request(core_service, preset.clone(), prompt_params).await
                })
            })
            .collect();
        let completed_snippets = join_all(tasks)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, JoinError>>()?
            .into_iter()
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        debug!("completed snippets: {:#?}", completed_snippets);

        self.apply_edit(completed_snippets).await?;

        self.notify_user(&progress_token, true).await?;

        Ok(())
    }
}
