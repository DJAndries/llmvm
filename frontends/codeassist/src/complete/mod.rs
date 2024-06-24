mod apply;
mod notify;
mod processing;
mod symbols;
mod typedefs;

use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

use anyhow::{anyhow, Result};
use llmvm_protocol::{
    service::{BoxedService, CoreRequest, CoreResponse, ServiceFuture},
    GenerationParameters, GenerationRequest, GenerationResponse, ServiceResponse,
};
use lsp_types::{Location, ServerCapabilities, Url};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tower::Service;
use tracing::error;

use crate::{content::ContentManager, service::LspMessageService, CodeAssistConfig};

use self::processing::IdentifiedGenerationResponseStream;

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
    response: GenerationResponse,
}

struct CompletingSnippet {
    preset: String,
    stream: IdentifiedGenerationResponseStream,
}

pub struct CodeCompleteTask {
    config: Arc<CodeAssistConfig>,

    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
    passthrough_service: LspMessageService,
    root_uri: Option<Url>,
    supports_semantic_tokens: bool,
    supports_folding_ranges: bool,
    supports_type_definitions: bool,

    content_manager: Arc<Mutex<ContentManager>>,
    code_location: Location,

    task_id: usize,
    random_context_locations: Vec<Location>,

    notify_complete_status: Arc<Mutex<bool>>,

    thread_id: Arc<Mutex<Option<String>>>,
}

impl CodeCompleteTask {
    pub fn new(
        config: Arc<CodeAssistConfig>,
        llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
        passthrough_service: LspMessageService,
        server_capabilities: Option<&ServerCapabilities>,
        root_uri: Option<Url>,
        content_manager: Arc<Mutex<ContentManager>>,
        code_location: Location,
        task_id: usize,
        random_context_locations: Vec<Location>,
        thread_id: Arc<Mutex<Option<String>>>,
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
            notify_complete_status: Default::default(),
            thread_id,
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
        &mut self,
        preset: String,
        prompt_params: Value,
        should_stream: bool,
        is_multiple_preset_request: bool,
    ) -> ServiceFuture<ServiceResponse<CoreResponse>> {
        let request = GenerationRequest {
            parameters: Some(GenerationParameters {
                prompt_parameters: Some(prompt_params),
                ..Default::default()
            }),
            preset_id: Some(preset.clone()),
            save_thread: self.config.use_chat_threads && !is_multiple_preset_request,
            existing_thread_id: self.thread_id.lock().await.clone(),
            ..Default::default()
        };
        let request = match should_stream {
            true => CoreRequest::GenerationStream(request),
            false => CoreRequest::Generation(request),
        };
        self.llmvm_core_service.lock().await.call(request)
    }

    pub async fn run(mut self) -> Result<()> {
        let progress_token = self.create_progress_token().await?;
        self.notify_user(&progress_token, None).await?;

        let result = self.process().await;
        if let Err(e) = result.as_ref() {
            error!("code complete task failed: {}", e);
        }

        self.notify_user(&progress_token, Some(result)).await?;
        Ok(())
    }
}
