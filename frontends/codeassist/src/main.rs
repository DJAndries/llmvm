use std::{
    env,
    process::{exit, Stdio},
};

use interceptor::LspInterceptor;
use passthrough::LspStdioPassthrough;
use tokio::{
    io::{stdin, stdout, Stdin},
    process::Command,
};

mod actions;
mod interceptor;
mod jsonrpc;
mod lsp;
mod passthrough;
mod service;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().collect();

    let mut child = Command::new(args[1].as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut passthrough = LspStdioPassthrough::new(
        stdin(),
        stdout(),
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
    );

    let mut interceptor = LspInterceptor::new(passthrough.get_service());

    passthrough.set_interceptor_service(interceptor.get_service());

    let interceptor_handle = tokio::spawn(async move { interceptor.run().await });

    passthrough.run().await?;

    interceptor_handle.await.unwrap();

    if let Some(status_code) = child.wait().await?.code() {
        exit(status_code)
    }

    Ok(())
}
