use std::{
    env,
    process::{exit, Stdio},
    sync::Arc,
};

use adapter::LspAdapter;
use anyhow::{anyhow, Context, Result};
use llmvm_protocol::{
    http::client::HttpClientConfig,
    service::{util::build_core_service_from_config, BoxedService},
    stdio::client::StdioClientConfig,
    ConfigExampleSnippet,
};
use llmvm_util::logging::setup_subscriber;
use llmvm_util::{config::load_config, generate_client_id};
use passthrough::LspStdioPassthrough;
use serde::Deserialize;
use session::{SessionSubscription, CODEASSIST_CLIENT_PREFIX};
use tokio::{
    io::{stdin, stdout},
    process::Command,
    sync::Mutex,
};
use tracing::error;

mod adapter;
mod complete;
mod content;
mod interceptor;
mod lsp;
mod passthrough;
mod service;
mod session;
mod tools;
mod util;

const CONFIG_FILENAME: &str = "codeassist.toml";
const LOG_FILENAME: &str = "codeassist.log";
const DEFAULT_PRESET: &str = "gpt-3.5-codegen";

#[derive(Deserialize)]
#[serde(default)]
pub struct CodeAssistConfig {
    tracing_directive: Option<String>,

    stdio_core: Option<StdioClientConfig>,
    http_core: Option<HttpClientConfig>,

    prefer_insert_in_place: bool,
    default_preset: String,

    stream_snippets: bool,
    use_chat_threads: bool,

    enable_tools: bool,
    use_native_tools: bool,
}

impl ConfigExampleSnippet for CodeAssistConfig {
    fn config_example_snippet() -> String {
        format!(
            r#"# The logging directive (see tracing crate for details); i.e. info, debug, etc.
# tracing_directive = "info"

# Prefer inserting the code completion "in place"; will
# only do so if one preset is being used during completion
# prefer_insert_in_place = false

# Default preset to use for code completion
# default_preset = "gpt-3.5-codegen"

# Stream completed code text to editor
# stream_snippets = false

# Stream completed code text to editor
# stream_snippets = false

# Use chat threads to keep context from previous requests
# use_chat_threads = true

# Enable tools/function calling to allow edits from the chat app
# enable_tools = false

# If tools are enabled, use native function calling instead of using results
# in text response (not recommended)
# use_native_tools = false

# Stdio core client configuration
# [stdio_core]
{}

# HTTP core client config
# [http_core]
{}"#,
            StdioClientConfig::config_example_snippet(),
            HttpClientConfig::config_example_snippet(),
        )
    }
}

impl Default for CodeAssistConfig {
    fn default() -> Self {
        Self {
            tracing_directive: None,
            stdio_core: None,
            http_core: None,
            prefer_insert_in_place: false,
            default_preset: DEFAULT_PRESET.to_string(),
            stream_snippets: false,
            use_chat_threads: true,
            enable_tools: false,
            use_native_tools: false,
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

    let llmvm_core_service: Arc<Mutex<BoxedService<_, _>>> = Arc::new(Mutex::new(
        build_core_service_from_config(config.stdio_core.take(), config.http_core.take())
            .await
            .map_err(|e| anyhow!(e))?,
    ));

    let passthrough_service = passthrough.get_service();

    let enable_tools = config.enable_tools;
    let use_native_tools = config.use_native_tools;

    let session_id = std::env::current_dir()?.to_string_lossy().into_owned();
    let client_id = generate_client_id(CODEASSIST_CLIENT_PREFIX);

    let mut adapter = LspAdapter::new(
        Arc::new(config),
        passthrough_service.clone(),
        llmvm_core_service.clone(),
        session_id.clone(),
        client_id.clone(),
    );

    passthrough.set_adapter_service(adapter.get_service());

    let adapter_handle = tokio::spawn(async move { adapter.run().await });

    if enable_tools {
        let session_subscription = SessionSubscription::new(
            llmvm_core_service,
            passthrough_service,
            session_id,
            client_id,
            use_native_tools,
        );
        tokio::spawn(async move {
            if let Err(e) = session_subscription.run().await {
                error!("failed to subscribe to session, tools disabled: {e}");
            }
        });
    }

    passthrough.run().await?;

    adapter_handle.await?;

    if let Some(status_code) = child.wait().await?.code() {
        exit(status_code)
    }

    Ok(())
}
