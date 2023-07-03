use std::io::ErrorKind;
use std::sync::Arc;
use std::{io, process::exit};

use clap::{command, Parser};
use llmvm_backend_util::{run_backend, BackendCommand};
use llmvm_llmrs::{LlmrsBackend, LlmrsConfig};
use llmvm_protocol::http::server::HttpServerConfig;
use llmvm_protocol::stdio::server::StdioServerConfig;
use llmvm_util::{config::load_config, logging::setup_subscriber};
use serde::Deserialize;

const CONFIG_FILENAME: &str = "llmrs.toml";
const LOG_FILENAME: &str = "llmrs.log";

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
    stdio_server: Option<StdioServerConfig>,
    http_server: Option<HttpServerConfig>,

    #[serde(flatten)]
    lib_config: LlmrsConfig,
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

    let mut backend = LlmrsBackend::new(config.lib_config);

    backend.load().await;

    run_backend(
        cli.command,
        Arc::new(backend),
        config.stdio_server,
        config.http_server,
    )
    .await
    .map_err(|e| io::Error::new(ErrorKind::Other, e))
}
