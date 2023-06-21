mod model;

use std::{collections::HashMap, str::FromStr, sync::Arc};

use futures::StreamExt;
use llm::{InferenceSessionConfig, LoadError, UnsupportedModelArchitecture};
use llmvm_protocol::{
    async_trait, Backend, BackendGenerationRequest, BackendGenerationResponse, ModelDescription,
    NotificationStream, ProtocolError, ProtocolErrorType,
};

use model::LlmrsModel;
use serde::Deserialize;
use thiserror::Error;
use tokio::{sync::RwLock, task::JoinHandle};
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
            LlmrsError::ModelNotReady => ProtocolErrorType::BadRequest,
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

    task_handles: RwLock<Vec<JoinHandle<()>>>,
    models: Arc<RwLock<HashMap<String, Option<LlmrsModel>>>>,
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

    async fn get_model<'a>(
        models: &'a HashMap<String, Option<LlmrsModel>>,
        request: &BackendGenerationRequest,
    ) -> Result<&'a LlmrsModel> {
        let model_description = ModelDescription::from_str(&request.model)
            .map_err(|_| LlmrsError::ModelDescriptionParse)?;
        Ok(models
            .get(&model_description.model_name)
            .ok_or(LlmrsError::WeightsNotFound)?
            .as_ref()
            .ok_or(LlmrsError::ModelNotReady)?)
    }

    pub async fn load(&self) {
        for weights_config in &self.config.weights {
            let name = weights_config.name.clone();
            self.models.write().await.insert(name, None);
            let models = self.models.clone();
            let weights_config = weights_config.clone();
            let task_handle = tokio::spawn(async move {
                let name = weights_config.name.clone();
                match LlmrsModel::load(weights_config).await {
                    Err(e) => {
                        error!("model load failed: {}", e);
                    }
                    Ok(model) => {
                        models.write().await.insert(name, Some(model));
                    }
                }
            });
            self.store_task_handle(task_handle).await;
        }
    }

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
            let models = self.models.read().await;
            let model = Self::get_model(&models, &request).await?;
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
            let models = self.models.read().await;
            let model = Self::get_model(&models, &request).await?;
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
