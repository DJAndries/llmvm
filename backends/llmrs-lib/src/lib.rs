//! [llmvm](https://github.com/djandries/llmvm) backend for [llm.rs](https://github.com/rustformers/llm).
//! Supports local language models such as llama, gpt-2, mpt and more.

mod model;

use std::{collections::HashMap, str::FromStr, sync::Arc};

use futures::StreamExt;
use llm::{InferenceSessionConfig, LoadError, UnsupportedModelArchitecture};

use llmvm_protocol::{
    async_trait, error::ProtocolErrorType, Backend, BackendGenerationRequest,
    BackendGenerationResponse, ConfigExampleSnippet, ModelDescription, NotificationStream,
    ProtocolError,
};
use model::LlmrsModel;
use serde::Deserialize;
use thiserror::Error;
use tokio::{
    sync::{Notify, OnceCell, RwLock},
    task::JoinHandle,
};
use tracing::error;

pub type Result<T> = std::result::Result<T, LlmrsError>;

/// Error enum containing all possible backend errors.
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
    #[error("model not ready")]
    ModelNotReady,
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
            LlmrsError::ModelNotReady => ProtocolErrorType::Internal,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

/// Weights/model configuration structure.
#[derive(Clone, Deserialize)]
pub struct LlmrsWeightsConfig {
    /// Name of the model.
    pub name: String,
    /// Architecture of the model. i.e. llama, mpt, etc.
    pub architecture: String,
    /// Max context token count for the model.
    pub context_tokens: usize,
    /// llm.rs inference session configuration.
    #[serde(default)]
    pub inference_session_config: InferenceSessionConfig,
}

/// Configuration structure for the backend.
#[derive(Clone, Deserialize)]
pub struct LlmrsConfig {
    /// All available weight configurations for the backend.
    pub weights: Vec<LlmrsWeightsConfig>,
}

impl ConfigExampleSnippet for LlmrsConfig {
    fn config_example_snippet() -> String {
        r#"# [[weights]]
# name of the weights
# name = "weights1"

# architecture of the weights
# architecture = "llama"

# number of context tokens
# context_tokens = 2048

# inference session config (optional)
# [[weights.inference_session_config]]
# The type of the memory K tensor.
# memory_k_type = "Float32"

# The type of the memory V tensor.
# memory_v_type = "Float32"

# Whether to use GPU acceleration
# use_gpu = false"#
            .into()
    }
}

/// A llmvm backend that uses llm.rs under the hood.
pub struct LlmrsBackend {
    config: LlmrsConfig,

    task_handles: RwLock<Vec<JoinHandle<()>>>,
    models: HashMap<String, Arc<(OnceCell<Option<LlmrsModel>>, Notify)>>,
}

impl LlmrsBackend {
    pub fn new(config: LlmrsConfig) -> Self {
        Self {
            config,
            task_handles: Default::default(),
            models: Default::default(),
        }
    }

    async fn store_task_handle(&self, handle: JoinHandle<()>) {
        let mut task_handles = self.task_handles.write().await;
        task_handles.retain(|h| !h.is_finished());
        task_handles.push(handle);
    }

    async fn get_model<'a>(&'a self, request: &BackendGenerationRequest) -> Result<&'a LlmrsModel> {
        let model_description = ModelDescription::from_str(&request.model)
            .map_err(|_| LlmrsError::ModelDescriptionParse)?;
        let model_entry = self
            .models
            .get(&model_description.model_name)
            .ok_or(LlmrsError::WeightsNotFound)?;
        loop {
            if let Some(model) = model_entry.0.get() {
                return match model {
                    Some(model) => Ok(model),
                    None => Err(LlmrsError::ModelNotReady),
                };
            }
            model_entry.1.notified().await
        }
    }

    /// Start loading models into memory.
    pub async fn load(&mut self) {
        for weights_config in &self.config.weights {
            let weights_config = weights_config.clone();
            let model_entry: Arc<(OnceCell<_>, Notify)> = Default::default();
            self.models
                .insert(weights_config.name.clone(), model_entry.clone());
            let task_handle = tokio::spawn(async move {
                model_entry
                    .0
                    .set(match LlmrsModel::load(weights_config).await {
                        Ok(model) => Some(model),
                        Err(e) => {
                            error!("model load failed: {}", e);
                            None
                        }
                    })
                    .ok();
                model_entry.1.notify_waiters();
            });
            self.store_task_handle(task_handle).await;
        }
    }

    /// Wait for model loading tasks (if any) to finish and
    /// close the backend.
    pub async fn close(&self) {
        for handle in self.task_handles.write().await.drain(..) {
            handle.await.expect("task should exit gracefully");
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
            let model = self.get_model(&request).await?;
            model.generate(request).await
        }
        .await
        .map_err(|e: LlmrsError| e.into())
    }

    async fn generate_stream(
        &self,
        request: BackendGenerationRequest,
    ) -> std::result::Result<NotificationStream<BackendGenerationResponse>, ProtocolError> {
        async {
            let model = self.get_model(&request).await?;
            let (handle, stream) = model.generate_stream(request).await;
            self.store_task_handle(handle).await;
            Ok(stream
                .map(|result| {
                    result
                        .map(|response| BackendGenerationResponse { response })
                        .map_err(|e| e.into())
                })
                .boxed())
        }
        .await
        .map_err(|e: LlmrsError| e.into())
    }
}
