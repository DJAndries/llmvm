use std::process::exit;
use std::sync::Arc;

use clap::{command, Parser};
use llmvm_backend_util::{run_backend, BackendCommand};
use llmvm_outsource::{OutsourceBackend, OutsourceConfig};
use llmvm_util::config::load_config;

const CONFIG_FILENAME: &str = "outsource.toml";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<BackendCommand>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let config: OutsourceConfig = match load_config(CONFIG_FILENAME) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("failed to load config: {}", e);
            exit(1);
        }
    };

    let cli = Cli::parse();

    run_backend(cli.command, Arc::new(OutsourceBackend::new(config))).await
}
