use std::{pin::Pin, task::Poll};

use anyhow::{anyhow, bail, Result};
use futures::{future::join_all, Stream};
use llmvm_protocol::{
    service::CoreResponse, GenerationResponse, NotificationStream, ServiceResponse,
};
use serde_json::Value;
use tokio::task::JoinError;
use tracing::{debug, error};

use crate::complete::{CompletedSnippet, PromptParameters};

use super::{CodeCompleteTask, CompletingSnippet, ContextSnippet};

pub(super) struct IdentifiedGenerationResponse {
    pub id: usize,
    pub response: Result<GenerationResponse>,
}

pub(super) struct IdentifiedGenerationResponseStream {
    pub id: usize,
    pub stream: NotificationStream<CoreResponse>,
}

#[derive(Default)]
pub(super) struct ProcessResult {
    pub typedef_infer_success: bool,
}

impl Stream for IdentifiedGenerationResponseStream {
    type Item = IdentifiedGenerationResponse;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.stream.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(resp) => Poll::Ready(resp.map(|r| {
                let r = r.map_err(|e| anyhow!(e)).and_then(|r| match r {
                    CoreResponse::GenerationStream(r) => Ok(r),
                    _ => Err(anyhow!("unexpected response from llmvm")),
                });
                IdentifiedGenerationResponse {
                    id: self.id,
                    response: r,
                }
            })),
        }
    }
}

impl CodeCompleteTask {
    async fn process_whole(&mut self, presets: Vec<String>, prompt_params: Value) -> Result<()> {
        let mut tasks = Vec::new();
        for preset in presets {
            let response_future = self
                .send_generation_request(preset.clone(), prompt_params.clone(), false)
                .await;
            tasks.push(tokio::spawn(async move {
                let response = response_future.await.map_err(|e| anyhow!(e))?;
                match response {
                    ServiceResponse::Single(response) => match response {
                        CoreResponse::Generation(response) => Ok(CompletedSnippet {
                            preset,
                            snippet: response.response,
                        }),
                        _ => bail!("unexpected response from llmvm"),
                    },
                    _ => bail!("unexpected service response type from llmvm"),
                }
            }));
        }

        let completed_snippets = join_all(tasks)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, JoinError>>()?
            .into_iter()
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

        debug!("completed snippets: {:#?}", completed_snippets);

        self.apply_completed_snippet_edits(completed_snippets)
            .await?;

        Ok(())
    }

    async fn process_stream(&mut self, presets: Vec<String>, prompt_params: Value) -> Result<()> {
        let mut completing_snippets = Vec::new();
        for (id, preset) in presets.into_iter().enumerate() {
            let response = self
                .send_generation_request(preset.clone(), prompt_params.clone(), true)
                .await
                .await
                .map_err(|e| anyhow!(e))?;
            match response {
                ServiceResponse::Multiple(stream) => {
                    completing_snippets.push(CompletingSnippet {
                        preset: preset.clone(),
                        stream: IdentifiedGenerationResponseStream { id, stream },
                    });
                }
                _ => bail!("unexpected service response type from llmvm"),
            }
        }

        self.apply_completing_snippet_edits(completing_snippets)
            .await?;

        Ok(())
    }

    async fn infer_typedef_context(&mut self) -> Result<Vec<ContextSnippet>> {
        Ok(if self.supports_type_definitions {
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
        })
    }

    pub(super) async fn process(&mut self) -> Result<ProcessResult> {
        self.content_manager
            .lock()
            .await
            .maybe_load_file(&self.code_location.uri)
            .await?;

        let mut process_result = ProcessResult::default();

        let typedef_context_snippets = match self.infer_typedef_context().await {
            Ok(result) => {
                process_result.typedef_infer_success = true;
                result
            }
            Err(e) => {
                error!("failed to infer typedef context: {e}");
                process_result.typedef_infer_success = false;
                Default::default()
            }
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

        let prompt_params = serde_json::to_value(PromptParameters {
            lang: self
                .content_manager
                .lock()
                .await
                .get_lang_id(&self.code_location.uri),
            typedef_context_snippets,
            random_context_snippets,
            main_snippet,
        })?;

        if self.config.stream_snippets {
            self.process_stream(presets, prompt_params).await?;
        } else {
            self.process_whole(presets, prompt_params).await?;
        }

        Ok(process_result)
    }
}
