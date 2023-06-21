use std::str::FromStr;
use std::sync::Arc;

use crate::{LlmrsError, LlmrsWeightsConfig, Result};
use llm::{
    InferenceParameters, InferenceRequest, InferenceSession, LoadProgress, Model,
    ModelArchitecture, ModelParameters, OutputRequest, Prompt,
};
use llmvm_protocol::{BackendGenerationRequest, BackendGenerationResponse};
use llmvm_util::get_file_path;
use rand::thread_rng;
use serde::{Deserialize, Serialize};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, info};

const WEIGHT_FILENAME_EXT: &str = ".bin";

pub struct LlmrsModel {
    model: Arc<Box<dyn Model>>,
    weights_config: LlmrsWeightsConfig,
}

#[derive(Serialize, Deserialize)]
struct SerializableInferenceParameters {
    n_threads: Option<usize>,
    n_batch: Option<usize>,
    top_k: Option<usize>,
    top_p: Option<f32>,
    repeat_penalty: Option<f32>,
    temperature: Option<f32>,
    repetition_penalty_last_n: Option<usize>,
}

impl Into<InferenceParameters> for SerializableInferenceParameters {
    fn into(self) -> InferenceParameters {
        let mut result = InferenceParameters::default();
        if let Some(n_threads) = self.n_threads {
            result.n_threads = n_threads;
        }
        if let Some(n_batch) = self.n_batch {
            result.n_batch = n_batch;
        }
        if let Some(top_k) = self.top_k {
            result.top_k = top_k;
        }
        if let Some(top_p) = self.top_p {
            result.top_p = top_p;
        }
        if let Some(repeat_penalty) = self.repeat_penalty {
            result.repeat_penalty = repeat_penalty;
        }
        if let Some(temperature) = self.temperature {
            result.temperature = temperature;
        }
        if let Some(n) = self.repetition_penalty_last_n {
            result.repetition_penalty_last_n = n;
        }
        result
    }
}

impl LlmrsModel {
    pub async fn load(weights_config: LlmrsWeightsConfig) -> Result<Self> {
        info!("Loading weights for {}", weights_config.name);
        let weights_filename = format!("{}{}", weights_config.name, WEIGHT_FILENAME_EXT);
        let weights_path = get_file_path(llmvm_util::DirType::Weights, &weights_filename, false)
            .ok_or(LlmrsError::UserHomeNotFound)?;
        let architecture = ModelArchitecture::from_str(&weights_config.architecture)?;
        let parameters = ModelParameters {
            context_size: weights_config.context_tokens,
            ..Default::default()
        };
        let name = weights_config.name.clone();
        let model = Arc::new(
            tokio::task::spawn_blocking(move || {
                llm::load_dynamic(architecture, &weights_path, parameters, None, |progress| {
                    match progress {
                        LoadProgress::TensorLoaded {
                            current_tensor,
                            tensor_count,
                        } => {
                            let percentage = (current_tensor as f64 / tensor_count as f64) * 100.0;
                            debug!("Load progress for {}: {:.2}%", name, percentage)
                        }
                        _ => (),
                    }
                })
            })
            .await
            .expect("model load task should join")?,
        );
        info!("Weights loaded for {}", weights_config.name);
        Ok(Self {
            model,
            weights_config,
        })
    }

    fn perform_inference(
        mut session: InferenceSession,
        model: Arc<Box<dyn Model>>,
        request: BackendGenerationRequest,
        response_tx: &UnboundedSender<Result<String>>,
    ) -> Result<()> {
        let inference_parameters: InferenceParameters =
            serde_json::from_value::<SerializableInferenceParameters>(
                serde_json::to_value(request.model_parameters.unwrap_or_default())
                    .expect("should convert model params to value"),
            )
            .map_err(|e| LlmrsError::InferenceParametersDeserialize(e))?
            .into();
        let inference_request = InferenceRequest {
            prompt: Prompt::Text(&request.prompt),
            parameters: &inference_parameters,
            play_back_previous_tokens: false,
            maximum_token_count: Some(request.max_tokens as usize),
        };
        let mut rng = thread_rng();
        session
            .infer::<LlmrsError>(
                model.as_ref().as_ref(),
                &mut rng,
                &inference_request,
                &mut OutputRequest::default(),
                |out| {
                    match out {
                        llm::InferenceResponse::InferredToken(out) => {
                            response_tx.send(Ok(out)).ok();
                        }
                        _ => (),
                    };
                    Ok(llm::InferenceFeedback::Continue)
                },
            )
            .map_err(|e| LlmrsError::Inference(e.to_string()))?;
        Ok(())
    }

    fn generate_helper(
        &self,
        request: BackendGenerationRequest,
    ) -> (JoinHandle<()>, UnboundedReceiver<Result<String>>) {
        let session = self
            .model
            .start_session(self.weights_config.inference_session_config.clone());
        let model = self.model.clone();
        let (response_tx, response_rx) = mpsc::unbounded_channel();
        let handle = tokio::task::spawn_blocking(move || {
            if let Err(e) = Self::perform_inference(session, model, request, &response_tx) {
                response_tx.send(Err(e)).ok();
            }
        });
        (handle, response_rx)
    }

    pub async fn generate(
        &self,
        request: BackendGenerationRequest,
    ) -> Result<BackendGenerationResponse> {
        let (handle, mut response_rx) = self.generate_helper(request);
        let mut response = String::new();
        while let Some(result) = response_rx.recv().await {
            match result {
                Ok(token) => response.push_str(&token),
                Err(e) => return Err(e),
            }
        }
        handle.await.expect("generate task handle should join");
        Ok(BackendGenerationResponse { response })
    }

    pub async fn generate_stream(
        &self,
        request: BackendGenerationRequest,
    ) -> (JoinHandle<()>, UnboundedReceiverStream<Result<String>>) {
        let (handle, response_rx) = self.generate_helper(request);
        (handle, UnboundedReceiverStream::new(response_rx))
    }
}
