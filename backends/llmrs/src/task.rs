use std::str::FromStr;

use crate::{LlmrsError, LlmrsWeightsConfig, Result};
use llm::{
    InferenceParameters, InferenceRequest, LoadProgress, Model, ModelArchitecture, ModelParameters,
    OutputRequest, Prompt,
};
use llmvm_protocol::{BackendGenerationRequest, BackendGenerationResponse};
use llmvm_util::get_file_path;
use rand::rngs::ThreadRng;
use rand::thread_rng;
use serde::{Deserialize, Serialize};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::{channel as oneshot_channel, Sender as OneshotSender};
use tracing::{debug, info};

const WEIGHT_FILENAME_EXT: &str = ".bin";

pub struct LlmrsTask {
    req_tx: Option<UnboundedSender<LlmrsRequest>>,
    req_rx: UnboundedReceiver<LlmrsRequest>,
    weights_config: LlmrsWeightsConfig,
}

pub struct LlmrsRequest {
    request: BackendGenerationRequest,
    reply_tx: OneshotSender<Result<BackendGenerationResponse>>,
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

impl LlmrsTask {
    pub fn new(weights_config: LlmrsWeightsConfig) -> Self {
        let (req_tx, req_rx) = unbounded_channel();
        Self {
            req_tx: Some(req_tx),
            req_rx,
            weights_config,
        }
    }

    pub fn get_sender(&mut self) -> UnboundedSender<LlmrsRequest> {
        self.req_tx.take().expect("sender should exist")
    }

    fn load(&mut self) -> Result<Box<dyn Model>> {
        info!("Loading weights for {}", self.weights_config.name);
        let weights_filename = format!("{}{}", self.weights_config.name, WEIGHT_FILENAME_EXT);
        let weights_path = get_file_path(llmvm_util::DirType::Weights, &weights_filename, false)
            .ok_or(LlmrsError::UserHomeNotFound)?;
        let architecture = ModelArchitecture::from_str(&self.weights_config.architecture)?;
        let parameters = ModelParameters {
            context_size: self.weights_config.context_tokens,
            ..Default::default()
        };
        let model = llm::load_dynamic(architecture, &weights_path, parameters, None, |progress| {
            match progress {
                LoadProgress::TensorLoaded {
                    current_tensor,
                    tensor_count,
                } => {
                    let percentage = (current_tensor as f64 / tensor_count as f64) * 100.0;
                    debug!(
                        "Load progress for {}: {:.2}%",
                        self.weights_config.name, percentage
                    )
                }
                _ => (),
            }
        })?;
        info!("Weights loaded for {}", self.weights_config.name);
        Ok(model)
    }

    fn process_request(
        &mut self,
        model: &Box<dyn Model>,
        rng: &mut ThreadRng,
        request: BackendGenerationRequest,
    ) -> Result<BackendGenerationResponse> {
        let mut session = model.start_session(self.weights_config.inference_session_config.clone());
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
        let mut response = String::new();
        session
            .infer::<LlmrsError>(
                model.as_ref(),
                rng,
                &inference_request,
                &mut OutputRequest::default(),
                |out| {
                    match out {
                        llm::InferenceResponse::InferredToken(out) => response.push_str(&out),
                        _ => (),
                    };
                    Ok(llm::InferenceFeedback::Continue)
                },
            )
            .map_err(|e| LlmrsError::Inference(e.to_string()))?;
        Ok(BackendGenerationResponse { response })
    }

    pub fn run(mut self) -> Result<()> {
        let mut rng = thread_rng();
        let model = self.load()?;
        while let Some(req) = self.req_rx.blocking_recv() {
            req.reply_tx
                .send(self.process_request(&model, &mut rng, req.request))
                .expect("should be able to send backend response back to main task");
        }
        debug!("Model task {} exiting...", self.weights_config.name);
        Ok(())
    }
}

pub async fn send_request_to_task(
    req_tx: &UnboundedSender<LlmrsRequest>,
    request: BackendGenerationRequest,
) -> Result<BackendGenerationResponse> {
    let (reply_tx, reply_rx) = oneshot_channel();
    let task_request = LlmrsRequest { request, reply_tx };
    req_tx
        .send(task_request)
        .map_err(|_| LlmrsError::RequestCannotSend)?;
    reply_rx.await.map_err(|_| LlmrsError::RequestCannotSend)?
}
