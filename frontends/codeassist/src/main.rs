use std::{
    env,
    process::{exit, Stdio},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use interceptor::LspInterceptor;
use llmvm_protocol::{
    services::{service_with_timeout, util::build_core_service_from_config, BoxedService},
    stdio::{CoreRequest, CoreResponse},
    HttpClientConfig,
};
use llmvm_util::config::load_config;
use llmvm_util::logging::setup_subscriber;
use passthrough::LspStdioPassthrough;
use serde::Deserialize;
use tokio::{
    io::{stdin, stdout},
    process::Command,
};
use tower::timeout::Timeout;

mod complete;
mod content;
mod interceptor;
mod lsp;
mod passthrough;
mod service;

const CONFIG_FILENAME: &str = "codeassist.toml";
const LOG_FILENAME: &str = "codeassist.log";
const DEFAULT_PRESET: &str = "gpt-3.5-codegen";

#[derive(Deserialize)]
#[serde(default)]
pub struct CodeAssistConfig {
    tracing_directive: Option<String>,
    bin_path: Option<String>,

    http_core: Option<HttpClientConfig>,

    prefer_insert_in_place: bool,
    default_preset: String,

    stream_snippets: bool,
}

impl Default for CodeAssistConfig {
    fn default() -> Self {
        Self {
            tracing_directive: None,
            bin_path: None,
            http_core: None,
            prefer_insert_in_place: false,
            default_preset: DEFAULT_PRESET.to_string(),
            stream_snippets: false,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = load_config::<CodeAssistConfig>(CONFIG_FILENAME).unwrap_or_else(|e| {
        eprintln!("failed to load config: {}", e);
        exit(1);
    });

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

    let llmvm_core_service: Timeout<BoxedService<_, _>> = service_with_timeout(
        build_core_service_from_config::<CoreRequest, CoreResponse>(
            config.bin_path.as_ref().map(|b| b.as_ref()),
            config.http_core.take(),
        )
        .await
        .map_err(|e| anyhow!(e))?,
    );

    let mut interceptor = LspInterceptor::new(
        Arc::new(config),
        passthrough.get_service(),
        llmvm_core_service,
    );

    passthrough.set_interceptor_service(interceptor.get_service());

    let interceptor_handle = tokio::spawn(async move { interceptor.run().await });

    passthrough.run().await?;

    interceptor_handle.await?;

    if let Some(status_code) = child.wait().await?.code() {
        exit(status_code)
    }

    Ok(())
}
