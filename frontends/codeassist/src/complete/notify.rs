use anyhow::{anyhow, Result};
use lsp_types::{
    notification::Progress, request::WorkDoneProgressCreate, ProgressParams, ProgressParamsValue,
    ProgressToken, WorkDoneProgress, WorkDoneProgressBegin, WorkDoneProgressCreateParams,
    WorkDoneProgressEnd, WorkDoneProgressReport,
};
use tower::Service;

use super::{processing::ProcessResult, CodeCompleteTask};
use crate::{lsp::LspMessage, service::LspMessageInfo};

const PROGRESS_TOKEN_PREFIX: &str = "llmvm/complete/";

impl CodeCompleteTask {
    pub(super) async fn create_progress_token(&mut self) -> Result<ProgressToken> {
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

    pub(super) async fn notify_user(
        &mut self,
        token: &ProgressToken,
        complete_result: Option<Result<ProcessResult>>,
    ) -> Result<()> {
        let progresses = match complete_result.as_ref() {
            None => vec![
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
            Some(complete_result) => {
                *self.notify_complete_status.lock().await = true;

                let message = match complete_result {
                    Ok(result) => if result.typedef_infer_success {
                        "Generation complete"
                    } else {
                        "Generation complete, but type definition inference failed (see logs)"
                    }
                    .to_string(),
                    Err(e) => format!("Generation failed: {e}"),
                };

                vec![
                    WorkDoneProgress::Report(WorkDoneProgressReport {
                        message: Some(message.clone()),
                        cancellable: Some(false),
                        ..Default::default()
                    }),
                    WorkDoneProgress::End(WorkDoneProgressEnd {
                        message: Some(message),
                    }),
                ]
            }
        };
        for (i, progress) in progresses.into_iter().enumerate() {
            let mut passthrough_service = self.passthrough_service.clone();
            let token = token.clone();
            let notify_complete_status = match complete_result.is_some() {
                false => Some(self.notify_complete_status.clone()),
                true => None,
            };
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(i as u64)).await;
                if let Some(status) = notify_complete_status {
                    if *status.lock().await {
                        return;
                    }
                }
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
}
