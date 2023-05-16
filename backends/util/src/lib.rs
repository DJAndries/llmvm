use std::error::Error;
use std::process::exit;
use std::sync::Arc;

use clap::{Args, Subcommand};
use llmvm_protocol::http::HttpServer;
use llmvm_protocol::services::BackendService;
use llmvm_protocol::stdio::StdioServer;
use llmvm_protocol::{Backend, BackendGenerationRequest, HttpServerConfig};
use tracing::error;

#[derive(Args, Clone)]
pub struct GenerateModelArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    prompt: String,

    #[arg(long)]
    max_tokens: u64,
}

#[derive(Args, Clone)]
pub struct HttpServerArgs {
    #[arg(short, long)]
    port: Option<u16>,
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
    Http(HttpServerArgs),
}

pub async fn run_backend<B: Backend + 'static>(
    command: Option<BackendCommand>,
    backend: Arc<B>,
    http_config: Option<HttpServerConfig>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // TODO: require a command to be specified, create --stdio switch
    // TODO: show error if stdio or http server features are not enabled
    match command {
        Some(command) => match command {
            BackendCommand::Generate(args) => {
                let result = backend
                    .generate(BackendGenerationRequest {
                        model: args.model,
                        prompt: args.prompt,
                        max_tokens: args.max_tokens,
                        thread_messages: None,
                        model_parameters: None,
                    })
                    .await;
                match result {
                    Ok(response) => {
                        println!("{}", response.response);
                    }
                    Err(e) => {
                        error!("Failed to process request: {}", e);
                        exit(1);
                    }
                };
            }
            BackendCommand::Http(args) => {
                let mut config = http_config.unwrap_or_default();
                if let Some(port) = args.port {
                    config.port = port;
                }
                HttpServer::new(BackendService::new(backend), config)
                    .run()
                    .await?;
            }
        },
        None => {
            StdioServer::new(BackendService::new(backend)).run().await?;
        }
    };
    Ok(())
}
