use std::sync::Arc;

use clap::{arg, command, Parser};
use llmvm_backend_util::{run_backend, BackendCommand};
use llmvm_outsource::OutsourceBackend;

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long)]
    api_key: Option<String>,

    #[command(subcommand)]
    command: Option<BackendCommand>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    run_backend(cli.command, Arc::new(OutsourceBackend::new(cli.api_key))).await
}
