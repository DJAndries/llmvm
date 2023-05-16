use std::io::ErrorKind;
use std::sync::Arc;
use std::{io, process::exit};

use clap::{command, Parser};
use llmvm_backend_util::{run_backend, BackendCommand};
use llmvm_outsource::{OutsourceBackend, OutsourceConfig};
use llmvm_protocol::HttpServerConfig;
use llmvm_util::{config::load_config, logging::setup_subscriber};
use serde::Deserialize;

const CONFIG_FILENAME: &str = "outsource.toml";
const LOG_FILENAME: &str = "outsource.log";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long)]
    log_to_file: bool,

    #[command(subcommand)]
    command: Option<BackendCommand>,
}

#[derive(Deserialize)]
struct CliConfigContent {
    tracing_directive: Option<String>,
    http_server: Option<HttpServerConfig>,

    #[serde(flatten)]
    lib_config: OutsourceConfig,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
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

    run_backend(
        cli.command,
        Arc::new(OutsourceBackend::new(config.lib_config)),
        config.http_server,
    )
    .await
    .map_err(|e| io::Error::new(ErrorKind::Other, e))
}
