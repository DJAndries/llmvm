mod task;

use std::{collections::HashMap, str::FromStr, thread::JoinHandle};

use llama_rs::{InferenceError, InferenceSessionParameters, LoadError, LoadProgress, Model};
use llmvm_protocol::{
    async_trait, Backend, BackendGenerationRequest, BackendGenerationResponse, ModelDescription,
    ProtocolError, ProtocolErrorType,
};
use rand::thread_rng;
use serde::Deserialize;
use std::thread;
use task::{send_request_to_task, LlamaRequest, LlamaTask};
use thiserror::Error;
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use tracing::{debug, error, info, instrument};

pub type Result<T> = std::result::Result<T, LlamaError>;

#[derive(Debug, Error)]
pub enum LlamaError {
    #[error("failed to parse model name")]
    ModelDescriptionParse,
    #[error("model weights not found")]
    WeightsNotFound,
    #[error("failed to send backend request to llama, model task may have crashed")]
    RequestCannotSend,
    #[error("failed to load model weights: {0}")]
    ModelLoad(#[from] LoadError),
    #[error("failed to deserialize inference parameters: {0}")]
    InferenceParametersDeserialize(serde_json::Error),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("unable to load app data, could not find user home folder")]
    UserHomeNotFound,
}

impl Into<ProtocolError> for LlamaError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            LlamaError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            LlamaError::WeightsNotFound => ProtocolErrorType::BadRequest,
            LlamaError::RequestCannotSend => ProtocolErrorType::Internal,
            LlamaError::ModelLoad(_) => ProtocolErrorType::Internal,
            LlamaError::InferenceParametersDeserialize(_) => ProtocolErrorType::BadRequest,
            LlamaError::Inference(_) => ProtocolErrorType::Internal,
            LlamaError::UserHomeNotFound => ProtocolErrorType::Internal,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct LlamaWeightsConfig {
    name: String,
    context_tokens: usize,
    #[serde(default)]
    inference_session_parameters: InferenceSessionParameters,
}

#[derive(Clone, Deserialize)]
pub struct LlamaConfig {
    weights: Vec<LlamaWeightsConfig>,
}

pub struct LlamaBackend {
    config: LlamaConfig,

    threads: RwLock<Vec<JoinHandle<()>>>,
    request_senders: RwLock<HashMap<String, UnboundedSender<LlamaRequest>>>,
}

impl LlamaBackend {
    pub fn new(config: LlamaConfig) -> Self {
        Self {
            config,
            threads: Default::default(),
            request_senders: Default::default(),
        }
    }

    pub async fn load(&self) {
        for weights_config in &self.config.weights {
            let mut task = LlamaTask::new(weights_config.clone());
            self.request_senders
                .write()
                .await
                .insert(weights_config.name.clone(), task.get_sender());
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
impl Backend for LlamaBackend {
    async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<BackendGenerationResponse, ProtocolError> {
        async {
            let model_description = ModelDescription::from_str(&request.model)
                .map_err(|_| LlamaError::ModelDescriptionParse)?;

            let request_senders = self.request_senders.read().await;
            let request_sender = request_senders
                .get(&model_description.model_name)
                .ok_or(LlamaError::WeightsNotFound)?;

            Ok(send_request_to_task(request_sender, request).await?)
        }
        .await
        .map_err(|e: LlamaError| e.into())
    }
}
