use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use futures::{stream::select, StreamExt};
use llmvm_protocol::{
    service::{CoreRequest, CoreResponse},
    tower::Service,
    BoxedService, ServiceResponse, SubscribeToThreadRequest, ThreadEvent, Tool,
};
use tokio::sync::Mutex;

use crate::{service::LspMessageService, tools::Tools};

pub const CODEGEN_TAG: &str = "codegen";
pub const CODECHAT_TAG: &str = "codechat";
pub const ALL_SESSION_TAGS: [&str; 2] = [CODEGEN_TAG, CODECHAT_TAG];
pub const CODEASSIST_CLIENT_PREFIX: &str = "codeassist";

pub struct SessionSubscription {
    llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
    tool_definitions: Vec<Tool>,
    tools: Tools,
    session_id: String,
    client_id: String,
}

impl SessionSubscription {
    pub fn new(
        llmvm_core_service: Arc<Mutex<BoxedService<CoreRequest, CoreResponse>>>,
        passthrough_service: LspMessageService,
        session_id: String,
        client_id: String,
        use_native_tools: bool,
    ) -> Self {
        Self {
            llmvm_core_service: llmvm_core_service.clone(),
            tool_definitions: Tools::get_definitions(use_native_tools),
            tools: Tools::new(
                session_id.clone(),
                client_id.clone(),
                llmvm_core_service,
                passthrough_service,
                use_native_tools,
            ),
            session_id,
            client_id,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let codegen_response = self
            .llmvm_core_service
            .lock()
            .await
            .call(CoreRequest::SubscribeToThread(SubscribeToThreadRequest {
                session_id: Some(self.session_id.clone()),
                session_tag: Some(CODEGEN_TAG.to_string()),
                client_id: self.client_id.clone(),
                tools: Some(self.tool_definitions.clone()),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow!(e))?;
        let codechat_response = self
            .llmvm_core_service
            .lock()
            .await
            .call(CoreRequest::SubscribeToThread(SubscribeToThreadRequest {
                session_id: Some(self.session_id.clone()),
                session_tag: Some(CODECHAT_TAG.to_string()),
                client_id: self.client_id.clone(),
                tools: Some(self.tool_definitions),
                ..Default::default()
            }))
            .await
            .map_err(|e| anyhow!(e))?;
        let codegen_stream = match codegen_response {
            ServiceResponse::Multiple(stream) => stream,
            _ => bail!("unexpected thread subscription response"),
        }
        .map(|v| (v, CODEGEN_TAG));
        let codechat_stream = match codechat_response {
            ServiceResponse::Multiple(stream) => stream,
            _ => bail!("unexpected thread subscription response"),
        }
        .map(|v| (v, CODECHAT_TAG));
        let mut event_stream = select(codegen_stream, codechat_stream);
        tokio::spawn(async move {
            while let Some((event, tag)) = event_stream.next().await {
                if let Ok(event) = event {
                    if let CoreResponse::ListenOnThread(event) = event {
                        if let ThreadEvent::Message { message } = event {
                            if let Some(tool_calls) = message.tool_calls {
                                self.tools.process_tool_calls(tool_calls, tag).await;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }
}
