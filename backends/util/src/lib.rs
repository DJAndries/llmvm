use std::io;
use std::process::exit;

use clap::{Args, Subcommand};
use llmvm_proto::{BackendGenerationRequest, BackendGenerationResponse};

#[derive(Args, Clone)]
pub struct ModelArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    prompt: String,

    #[arg(long)]
    max_tokens: u64,
}

impl Into<BackendGenerationRequest> for ModelArgs {
    fn into(self) -> BackendGenerationRequest {
        BackendGenerationRequest {
            model: self.model,
            prompt: self.prompt,
            max_tokens: self.max_tokens,
            ..Default::default()
        }
    }
}

#[derive(Subcommand)]
pub enum InputMode {
    Spec(ModelArgs),
    Json,
}

pub fn get_request(input_mode: &InputMode) -> BackendGenerationRequest {
    match input_mode {
        InputMode::Spec(args) => args.clone().into(),
        InputMode::Json => serde_json::from_reader(io::stdin()).unwrap_or_else(|e| {
            // TODO: replace with tracing::error
            eprintln!("Failed to deserialize JSON request: {}", e);
            exit(1);
        }),
    }
}

pub fn print_response(response: BackendGenerationResponse, input_mode: &InputMode) {
    println!(
        "{}",
        match input_mode {
            InputMode::Spec(_) => response.response,
            InputMode::Json => serde_json::to_string(&response).unwrap(),
        }
    );
}
