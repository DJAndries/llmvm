use std::{
    env,
    process::{exit, Stdio},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use interceptor::LspInterceptor;
use llmvm_protocol::stdio::StdioClient;
use llmvm_util::config::load_config;
use llmvm_util::logging::setup_subscriber;
use passthrough::LspStdioPassthrough;
use serde::Deserialize;
use tokio::{
    io::{stdin, stdout},
    process::Command,
};

mod complete;
mod content;
mod interceptor;
mod jsonrpc;
mod lsp;
mod passthrough;
mod service;

const LLMVM_CORE_CLI_COMMAND: &str = "llmvm-core-cli";
const CONFIG_FILENAME: &str = "codeassist.toml";
const LOG_FILENAME: &str = "codeassist.log";

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct CodeAssistConfig {
    tracing_directive: Option<String>,
    bin_path: Option<String>,

    prefer_insert_in_place: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config: Arc<CodeAssistConfig> = match load_config(CONFIG_FILENAME) {
        Ok(config) => Arc::new(config),
        Err(e) => {
            eprintln!("failed to load config: {}", e);
            exit(1);
        }
    };

    setup_subscriber(
        config.tracing_directive.as_ref().map(|d| d.as_str()),
        Some(LOG_FILENAME),
    );

    let args: Vec<String> = env::args().collect();

    let real_server_name = args
        .get(1)
        .ok_or(anyhow!("expected real server name in args"))?;
    let real_server_args = match args.len() > 2 {
        false => Vec::new(),
        true => args[2..].to_vec(),
    };
    let mut child = Command::new(real_server_name)
        .args(real_server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start real server")?;

    let mut passthrough = LspStdioPassthrough::new(
        stdin(),
        stdout(),
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
    );

    let llmvm_core_service = StdioClient::new(
        config.bin_path.as_ref().map(|d| d.as_ref()),
        LLMVM_CORE_CLI_COMMAND,
        &["--log-to-file".to_string(), "stdio-server".to_string()],
    )
    .await
    .context("failed to start llmvm-core-cli")?;

    let mut interceptor =
        LspInterceptor::new(config, passthrough.get_service(), llmvm_core_service);

    passthrough.set_interceptor_service(interceptor.get_service());

    let interceptor_handle = tokio::spawn(async move { interceptor.run().await });

    passthrough.run().await?;

    interceptor_handle.await?;

    if let Some(status_code) = child.wait().await?.code() {
        exit(status_code)
    }

    Ok(())
}
