use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use futures::{stream::select, StreamExt};
use llmvm_protocol::{
    service::{CoreRequest, CoreResponse},
    tower::Service,
    BoxedService, ServiceResponse, SubscribeToThreadRequest, ThreadEvent,
};
use llmvm_util::generate_client_id;
use tokio::sync::Mutex;

use crate::{service::LspMessageService, tools::Tools};

const CODEGEN_TAG: &str = "codegen";
const CODECHAT_TAG: &str = "codechat";
const CODEASSIST_CLIENT_PREFIX: &str = "codeassist";

pub struct SessionSubscription {
    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
    passthrough_service: Option<LspMessageService>,
}

impl SessionSubscription {
    pub fn new(
        llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
        passthrough_service: LspMessageService,
    ) -> Self {
        Self {
            llmvm_core_service,
            passthrough_service: Some(passthrough_service),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let session_id = std::env::current_dir()?.to_string_lossy().into_owned();
        let client_id = generate_client_id(CODEASSIST_CLIENT_PREFIX);

        let tool_definitions = Tools::get_definitions();
        let mut tools = Tools::new(client_id.clone(), self.passthrough_service.take().unwrap());

        let codegen_response = self
            .llmvm_core_service
            .lock()
            .await
            .call(CoreRequest::SubscribeToThread(SubscribeToThreadRequest {
                session_id: Some(session_id.clone()),
                session_tag: Some(CODEGEN_TAG.to_string()),
                client_id: client_id.clone(),
                tools: Some(tool_definitions.clone()),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow!(e))?;
        let codechat_response = self
            .llmvm_core_service
            .lock()
            .await
            .call(CoreRequest::SubscribeToThread(SubscribeToThreadRequest {
                session_id: Some(session_id.clone()),
                session_tag: Some(CODECHAT_TAG.to_string()),
                client_id: client_id.clone(),
                tools: Some(tool_definitions),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow!(e))?;
        let codegen_stream = match codegen_response {
            ServiceResponse::Multiple(stream) => stream,
            _ => bail!("unexpected thread subscription response"),
        };
        let codechat_stream = match codechat_response {
            ServiceResponse::Multiple(stream) => stream,
            _ => bail!("unexpected thread subscription response"),
        };
        let mut event_stream = select(codegen_stream, codechat_stream);
        tokio::spawn(async move {
            while let Some(event) = event_stream.next().await {
                if let Ok(event) = event {
                    if let CoreResponse::ListenOnThread(event) = event {
                        if let ThreadEvent::Message { message } = event {
                            if let Some(tool_calls) = message.tool_calls {
                                tools.process_tool_calls(tool_calls).await;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }
}
