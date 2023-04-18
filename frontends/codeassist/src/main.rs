use std::{
    env,
    process::{exit, Stdio},
};

use anyhow::{anyhow, Context, Result};
use interceptor::LspInterceptor;
use llmvm_protocol::stdio::StdioClient;
use passthrough::LspStdioPassthrough;
use tokio::{
    io::{stdin, stdout},
    process::Command,
};
use tracing_subscriber::EnvFilter;

mod complete;
mod content;
mod interceptor;
mod jsonrpc;
mod lsp;
mod passthrough;
mod service;

const LLMVM_CORE_CLI_COMMAND: &str = "llmvm-core-cli";

#[tokio::main]
async fn main() -> Result<()> {
    // TODO: consider writing logs to file
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

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

    let llmvm_core_service = StdioClient::new(LLMVM_CORE_CLI_COMMAND, &[])
        .await
        .context("failed to start llmvm-core-cli")?;

    let mut interceptor = LspInterceptor::new(passthrough.get_service(), llmvm_core_service);

    passthrough.set_interceptor_service(interceptor.get_service());

    let interceptor_handle = tokio::spawn(async move { interceptor.run().await });

    passthrough.run().await?;

    interceptor_handle.await?;

    if let Some(status_code) = child.wait().await?.code() {
        exit(status_code)
    }

    Ok(())
}
