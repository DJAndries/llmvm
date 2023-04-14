use std::error::Error;

use anyhow::{anyhow, Result};
use llmvm_protocol::stdio::{CoreRequest, CoreResponse, StdioClient};
use lsp_types::{
    request::{DocumentSymbolRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest},
    DocumentSymbolParams, PartialResultParams, Range, SemanticTokensDeltaParams,
    SemanticTokensParams, SemanticTokensRangeParams, Url, WorkDoneProgressParams,
};
use tower::{buffer::Buffer, timeout::Timeout, Service};

use crate::{
    lsp::{CodeCompleteParams, LspMessage},
    service::{LspMessageInfo, LspMessageService},
    snippet::SnippetFetcher,
};

pub struct CodeCompleteTask {
    // TODO: remove buffer once json-rpc is used in protocol
    // just clone the service directly instead
    llmvm_core_service: Buffer<Timeout<StdioClient<CoreRequest, CoreResponse>>, CoreRequest>,
    passthrough_service: LspMessageService,

    snippet_fetcher: SnippetFetcher,
    code_complete_params: CodeCompleteParams,
}

impl CodeCompleteTask {
    // TODO: add optional custom prompt
    pub fn new(
        llmvm_core_service: Buffer<Timeout<StdioClient<CoreRequest, CoreResponse>>, CoreRequest>,
        passthrough_service: LspMessageService,
        code_complete_params: CodeCompleteParams,
    ) -> Self {
        Self {
            llmvm_core_service,
            passthrough_service,
            snippet_fetcher: SnippetFetcher::default(),
            code_complete_params,
        }
    }

    async fn get_relevant_symbols(&mut self) -> Result<()> {
        eprintln!("getting relevent symbols");
        // let symbols_response = self
        //     .passthrough_service
        //     .call(LspMessageInfo::new(
        //         LspMessage::new_request::<SemanticTokensRangeRequest>(SemanticTokensRangeParams {
        //             work_done_progress_params: WorkDoneProgressParams::default(),
        //             partial_result_params: PartialResultParams::default(),
        //             text_document: self.code_complete_params.text_document.clone(),
        //             range: self.code_complete_params.range.clone(),
        //         })
        //         .unwrap(),
        //         false,
        //     ))
        //     .await
        //     .map_err(|e| anyhow!("failed to request lsp: {}", e))?
        //     .ok_or(anyhow!("symbols request should have response"))?;

        let symbols_response = self
            .passthrough_service
            .call(LspMessageInfo::new(
                LspMessage::new_request::<DocumentSymbolRequest>(DocumentSymbolParams {
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                    text_document: self.code_complete_params.text_document.clone(),
                })
                .unwrap(),
                false,
            ))
            .await
            .map_err(|e| anyhow!("failed to request lsp: {}", e))?
            .ok_or(anyhow!("symbols request should have response"))?;

        eprintln!("{:?}", symbols_response);

        Ok(())
    }

    pub async fn run(mut self) -> Result<()> {
        self.get_relevant_symbols().await
    }
}
