use std::process::exit;
use std::sync::Arc;

use clap::{command, Parser};
use llmvm_backend_util::{run_backend, BackendCommand};
use llmvm_llama::{LlamaBackend, LlamaConfig};
use llmvm_util::{config::load_config, logging::setup_subscriber};
use serde::Deserialize;

const CONFIG_FILENAME: &str = "llama.toml";
const LOG_FILENAME: &str = "llama.log";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long)]
    log_to_file: bool,

    #[command(subcommand)]
    command: Option<BackendCommand>,
}

#[derive(Deserialize)]
pub struct CliConfigContent {
    tracing_directive: Option<String>,

    #[serde(flatten)]
    lib_config: LlamaConfig,
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

    let backend = Arc::new(LlamaBackend::new(config.lib_config));

    backend.load().await;

    run_backend(cli.command, backend).await
}
