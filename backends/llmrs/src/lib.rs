mod task;

use std::{collections::HashMap, str::FromStr, thread::JoinHandle};

use llm::{InferenceSessionConfig, LoadError, UnsupportedModelArchitecture};
use llmvm_protocol::{
    async_trait, Backend, BackendGenerationRequest, BackendGenerationResponse, ModelDescription,
    ProtocolError, ProtocolErrorType,
};

use serde::Deserialize;
use std::thread;
use task::{send_request_to_task, LlmrsRequest, LlmrsTask};
use thiserror::Error;
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use tracing::error;

pub type Result<T> = std::result::Result<T, LlmrsError>;

#[derive(Debug, Error)]
pub enum LlmrsError {
    #[error("failed to parse model name")]
    ModelDescriptionParse,
    #[error("model weights not found")]
    WeightsNotFound,
    #[error("failed to send backend request to llmrs, model task may have crashed")]
    RequestCannotSend,
    #[error("failed to load model weights: {0}")]
    ModelLoad(#[from] LoadError),
    #[error("failed to deserialize inference parameters: {0}")]
    InferenceParametersDeserialize(serde_json::Error),
    #[error("bad model architecture type: {0}")]
    UnsupportedArchitecture(#[from] UnsupportedModelArchitecture),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("unable to load app data, could not find user home folder")]
    UserHomeNotFound,
}

impl Into<ProtocolError> for LlmrsError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            LlmrsError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            LlmrsError::WeightsNotFound => ProtocolErrorType::BadRequest,
            LlmrsError::RequestCannotSend => ProtocolErrorType::Internal,
            LlmrsError::UnsupportedArchitecture(_) => ProtocolErrorType::Internal,
            LlmrsError::ModelLoad(_) => ProtocolErrorType::Internal,
            LlmrsError::InferenceParametersDeserialize(_) => ProtocolErrorType::BadRequest,
            LlmrsError::Inference(_) => ProtocolErrorType::Internal,
            LlmrsError::UserHomeNotFound => ProtocolErrorType::Internal,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct LlmrsWeightsConfig {
    name: String,
    architecture: String,
    context_tokens: usize,
    #[serde(default)]
    inference_session_config: InferenceSessionConfig,
}

#[derive(Clone, Deserialize)]
pub struct LlmrsConfig {
    weights: Vec<LlmrsWeightsConfig>,
}

pub struct LlmrsBackend {
    config: LlmrsConfig,

    threads: RwLock<Vec<JoinHandle<()>>>,
    request_senders: RwLock<HashMap<String, UnboundedSender<LlmrsRequest>>>,
}

impl LlmrsBackend {
    pub fn new(config: LlmrsConfig) -> Self {
        Self {
            config,
            threads: Default::default(),
            request_senders: Default::default(),
        }
    }

    pub async fn load(&self) {
        for weights_config in &self.config.weights {
            let name = weights_config.name.clone();
            let mut task = LlmrsTask::new(weights_config.clone());
            self.request_senders
                .write()
                .await
                .insert(name, task.get_sender());
            self.threads.write().await.push(thread::spawn(move || {
                if let Err(e) = task.run() {
                    error!("model task failed: {}", e);
                }
            }));
        }
    }

    pub async fn close(&self) {
        self.request_senders.write().await.clear();
        for thread in self.threads.write().await.drain(..) {
            thread.join().expect("thread should exit gracefully");
        }
    }
}

#[async_trait]
impl Backend for LlmrsBackend {
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<BackendGenerationResponse, ProtocolError> {
        async {
            let model_description = ModelDescription::from_str(&request.model)
                .map_err(|_| LlmrsError::ModelDescriptionParse)?;

            let request_senders = self.request_senders.read().await;
            let request_sender = request_senders
                .get(&model_description.model_name)
                .ok_or(LlmrsError::WeightsNotFound)?;

            Ok(send_request_to_task(request_sender, request).await?)
        }
        .await
        .map_err(|e: LlmrsError| e.into())
    }
}
