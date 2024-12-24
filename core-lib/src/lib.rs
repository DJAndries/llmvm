//! The core components for [llmvm](https://github.com/djandries/llmvm).
//!
//! Assumes the following responsibilities:
//! - Sending generation requests to backends
//! - Managing message threads
//! - Model presets
//! - Prompt templates
//! - Projects

pub mod error;
pub mod generation;
pub mod presets;
pub mod prompts;
pub mod service;
mod sessions;
mod threads;
mod tools;

use llmvm_protocol::http::client::{HttpClient, HttpClientConfig};
use llmvm_protocol::service::{BackendRequest, BackendResponse};
use llmvm_protocol::stdio::client::{StdioClient, StdioClientConfig};
use llmvm_protocol::{BoxedService, ConfigExampleSnippet, ModelDescription};
use serde::Deserialize;
use threads::clean_old_threads;

use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::debug;

use crate::error::CoreError;

const BACKEND_COMMAND_PREFIX: &str = "llmvm-";
const PROJECT_DIR_NAME: &str = ".llmvm";
const DEFAULT_THREAD_TTL_SECS: u64 = 14 * 24 * 3600;

pub type Result<T> = std::result::Result<T, CoreError>;

/// Configuration structure for the llmvm core.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct LLMVMCoreConfig {
    /// Configuration for all backend stdio clients.
    pub stdio_client: Option<StdioClientConfig>,
    /// The max time-to-live for threads in seconds.
    /// Threads with a last modified time older than the TTL will be removed.
    pub thread_ttl_secs: Option<u64>,
    /// HTTP client configurations for remote backends.
    pub http_backends: HashMap<String, HttpClientConfig>,
}

impl ConfigExampleSnippet for LLMVMCoreConfig {
    fn config_example_snippet() -> String {
        format!(
            r#"# TTL in seconds for message threads (based on last modified time)
# thread_ttl_secs = 604800

# Stdio client configuration
# [stdio_client]
{}

# HTTP backend client config
# [http_backends.llmrs-http]
{}"#,
            StdioClientConfig::config_example_snippet(),
            HttpClientConfig::config_example_snippet(),
        )
    }
}

/// The llmvm core which handles sending generation requests to the backend,
/// managing message threads, model presets, and prompt templates. Frontends
/// request text generation by invoking the core.
pub struct LLMVMCore {
    clients: Mutex<HashMap<String, BoxedService<BackendRequest, BackendResponse>>>,
    config: LLMVMCoreConfig,
}

impl LLMVMCore {
    pub async fn new(mut config: LLMVMCoreConfig) -> Result<Self> {
        clean_old_threads(config.thread_ttl_secs.unwrap_or(DEFAULT_THREAD_TTL_SECS)).await?;

        let mut clients: HashMap<String, BoxedService<BackendRequest, BackendResponse>> =
            HashMap::new();

        for (name, config) in config.http_backends.drain() {
            debug!("loading {} http backend", name);
            clients.insert(
                name,
                Box::new(HttpClient::new(config).map_err(|_| CoreError::HttpServiceCreate)?),
            );
        }

        Ok(Self {
            clients: Mutex::new(clients),
            config,
        })
    }

    async fn get_client<'a>(
        &self,
        clients_guard: &'a mut HashMap<String, BoxedService<BackendRequest, BackendResponse>>,
        model_description: &ModelDescription,
    ) -> Result<&'a mut BoxedService<BackendRequest, BackendResponse>> {
        let command = format!("{}{}", BACKEND_COMMAND_PREFIX, model_description.backend);
        if !clients_guard.contains_key(&model_description.backend) {
            debug!(
                "starting backend {command} in {:?}",
                self.config.stdio_client.as_ref().map(|c| &c.bin_path)
            );
            let backend = model_description.backend.as_str();
            clients_guard.insert(
                backend.to_string(),
                Box::new(
                    StdioClient::new(
                        &command,
                        &["--log-to-file"],
                        self.config.stdio_client.clone().unwrap_or_default(),
                    )
                    .await
                    .map_err(|e| CoreError::StdioBackendStart(e))?,
                ),
            );
        }
        Ok(clients_guard.get_mut(&model_description.backend).unwrap())
    }

    /// Close the active stdio (local child process) or HTTP client (remote process)
    /// for a given backend. If the backend client is over stdio, the backend child process
    /// will be killed.
    pub async fn close_client(&self, backend_name: &str) {
        self.clients.lock().await.remove(backend_name);
    }
}
