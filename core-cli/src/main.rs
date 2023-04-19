use std::{process::exit, sync::Arc};

use clap::{arg, command, Args, Parser, Subcommand};
use llmvm_core::LLMVMCore;
use llmvm_protocol::{
    stdio::{CoreService, StdioServer},
    Core, GenerationRequest,
};
use llmvm_util::config::load_config;
use llmvm_util::logging::setup_subscriber;
use serde::Deserialize;

const CONFIG_FILENAME: &str = "core.toml";
const LOG_FILENAME: &str = "core.log";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<CoreCommand>,

    #[arg(long)]
    log_to_file: bool,
}

#[derive(Subcommand)]
pub enum CoreCommand {
    Generate(GenerateArgs),
}

#[derive(Args, Clone)]
pub struct GenerateArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    prompt: String,

    #[arg(long)]
    model_parameters_preset_id: Option<String>,

    #[arg(long)]
    existing_thread_id: Option<u64>,

    #[arg(long)]
    save_thread: bool,

    #[arg(long)]
    max_tokens: u64,
}

#[derive(Deserialize)]
pub struct CliConfigContent {
    tracing_directive: Option<String>,

    bin_path: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let config: CliConfigContent = match load_config(CONFIG_FILENAME) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("failed to load config: {}", e);
            exit(1);
        }
    };

    let cli = Cli::parse();

    setup_subscriber(
        config.tracing_directive.as_ref().map(|d| d.as_str()),
        if cli.log_to_file {
            Some(LOG_FILENAME)
        } else {
            None
        },
    );

    let core = Arc::new(match LLMVMCore::new(config.bin_path).await {
        Ok(core) => core,
        Err(e) => {
            eprintln!("failed to init core: {}", e);
            exit(1);
        }
    });

    match cli.command {
        Some(command) => match command {
            CoreCommand::Generate(args) => {
                let request = GenerationRequest {
                    model: args.model,
                    prompt_template_id: None,
                    custom_prompt_template: Some(args.prompt),
                    max_tokens: args.max_tokens,
                    model_parameters_preset_id: args.model_parameters_preset_id,
                    model_parameters: None,
                    prompt_parameters: Default::default(),
                    existing_thread_id: args.existing_thread_id,
                    save_thread: args.save_thread,
                };
                match core.generate(request).await {
                    Err(e) => {
                        eprintln!("failed to generate: {}", e);
                        exit(1);
                    }
                    Ok(response) => {
                        println!("{}", response.response);
                        if let Some(id) = response.thread_id {
                            eprintln!("Thread ID is {}", id);
                        }
                    }
                }
            }
        },
        None => {
            StdioServer::new(CoreService::new(core)).run().await?;
        }
    };
    Ok(())
}
