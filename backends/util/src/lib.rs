use std::process::exit;
use std::sync::Arc;

use clap::{Args, Subcommand};
use llmvm_protocol::stdio::{BackendService, StdioServer};
use llmvm_protocol::{Backend, BackendGenerationRequest};

#[derive(Args, Clone)]
pub struct GenerateModelArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    prompt: String,

    #[arg(long)]
    max_tokens: u64,
}

impl Into<BackendGenerationRequest> for GenerateModelArgs {
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
pub enum BackendCommand {
    Generate(GenerateModelArgs),
}

pub async fn run_backend<B: Backend + 'static>(
    command: Option<BackendCommand>,
    backend: Arc<B>,
) -> std::io::Result<()> {
    match command {
        Some(command) => {
            let result = match command {
                BackendCommand::Generate(args) => backend.generate(BackendGenerationRequest {
                    model: args.model,
                    prompt: args.prompt,
                    max_tokens: args.max_tokens,
                    thread_messages: None,
                    model_parameters: None,
                }),
            }
            .await;
            match result {
                Ok(response) => {
                    println!("{}", response.response);
                }
                Err(e) => {
                    // TODO: replace with tracing::error
                    eprintln!("Failed to process request: {}", e);
                    exit(1);
                }
            };
        }
        None => {
            StdioServer::new(BackendService::new(backend)).run().await?;
        }
    };
    Ok(())
}
