use std::{
    env,
    io::{stdout, Stdout, Write},
    process::exit,
};

use anyhow::{anyhow, Result};
use clap::Parser;
use futures::StreamExt;
use llmvm_protocol::{
    http::client::HttpClientConfig,
    service::{util::build_core_service_from_config, BoxedService, CoreRequest, CoreResponse},
    stdio::client::StdioClientConfig,
    ConfigExampleSnippet, GenerationParameters, GenerationRequest, ListenOnThreadRequest, Message,
    MessageRole, ServiceResponse,
};
use llmvm_util::{config::load_config, generate_client_id};
use rustyline::{error::ReadlineError, Config as RlConfig, DefaultEditor as RlEditor, EditMode};
use serde::Deserialize;
use serde_json::json;

type CoreService = BoxedService<CoreRequest, CoreResponse>;

const CONFIG_FILENAME: &str = "chat.toml";
const MESSAGE_PROMPT_PARAM: &str = "user_message";

const NO_COLOR_ENV_VAR: &str = "NO_COLOR";
const USER_ROLE_PREFIX: &str = "\x1b[1;36myou";
const ASSISTANT_ROLE_PREFIX: &str = "\x1b[1;33massistant";
const RESET_COLOR: &str = "\x1b[0m";
const USER_ROLE_NO_COLOR_PREFIX: &str = "you";
const ASSISTANT_ROLE_NO_COLOR_PREFIX: &str = "assistant";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(short, long, default_value = "gpt-3.5-chat")]
    preset: Option<String>,

    #[arg(short, long)]
    model: Option<String>,

    #[arg(short, long)]
    load_last_chat: bool,

    #[arg(short = 't', long)]
    load_thread_id: Option<String>,

    /// Enable vi mode
    #[arg(long)]
    vi: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ChatConfig {
    stdio_core: Option<StdioClientConfig>,
    http_core: Option<HttpClientConfig>,
}

impl ConfigExampleSnippet for ChatConfig {
    fn config_example_snippet() -> String {
        format!(
            r#"# Stdio core client configuration
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

struct ChatApp {
    cli: Cli,
    rl: RlEditor,
    llmvm_core_service: CoreService,
    thread_id: Option<String>,
    client_id: String,
    stdout: Stdout,
    listener_active: bool,
}

fn get_output_prefix(role: MessageRole, client_id: Option<&String>) -> String {
    let no_color = !env::var(NO_COLOR_ENV_VAR).unwrap_or_default().is_empty();
    let role_str = match role {
        MessageRole::System => panic!("no output prefix for system role"),
        MessageRole::Assistant => match no_color {
            false => ASSISTANT_ROLE_PREFIX,
            true => ASSISTANT_ROLE_NO_COLOR_PREFIX,
        },
        MessageRole::User => match no_color {
            false => USER_ROLE_PREFIX,
            true => USER_ROLE_NO_COLOR_PREFIX,
        },
    };
    match client_id {
        Some(client_id) => format!("{} ({}) > {}", role_str, client_id, RESET_COLOR),
        None => format!("{} > {}", role_str, RESET_COLOR),
    }
}

impl ChatApp {
    async fn new() -> Result<Self> {
        let cli = Cli::parse();

        let mut config = load_config::<ChatConfig>(CONFIG_FILENAME).unwrap_or_else(|e| {
            eprintln!("failed to load config: {}", e);
            exit(1);
        });

        let llmvm_core_service: CoreService =
            build_core_service_from_config(config.stdio_core.take(), config.http_core.take())
                .await
                .map_err(|e| anyhow!(e).context("failed to start core cli"))?;

        let rl = RlEditor::with_config(
            RlConfig::builder()
                .edit_mode(match cli.vi {
                    true => EditMode::Vi,
                    false => EditMode::Emacs,
                })
                .build(),
        )
        .expect("should be able to create rustyline editor");
        let stdout = stdout();

        let mut new = Self {
            cli,
            rl,
            llmvm_core_service,
            thread_id: None,
            stdout,
            client_id: generate_client_id("chat"),
            listener_active: false,
        };
        new.thread_id = if new.cli.load_last_chat {
            new.get_last_thread_id().await?
        } else {
            new.cli.load_thread_id.clone()
        };
        Ok(new)
    }

    async fn output_existing_messages(&mut self) -> Result<()> {
        if let Some(id) = self.thread_id.as_ref() {
            for message in self.get_thread_messages(id.clone()).await? {
                let prefix = get_output_prefix(message.role, message.client_id.as_ref());
                println!("{}{}", prefix, message.content);
            }
        }
        Ok(())
    }

    async fn get_last_thread_id(&mut self) -> Result<Option<String>> {
        let response = self
            .llmvm_core_service
            .call(CoreRequest::GetLastThreadInfo)
            .await
            .map_err(|e| anyhow!(e))?;
        match response {
            ServiceResponse::Single(response) => match response {
                CoreResponse::GetLastThreadInfo(info) => return Ok(info.map(|i| i.id)),
                _ => (),
            },
            _ => (),
        }
        Err(anyhow!("unexpected response while getting thread info"))
    }

    async fn get_thread_messages(&mut self, id: String) -> Result<Vec<Message>> {
        let response = self
            .llmvm_core_service
            .call(CoreRequest::GetThreadMessages { id })
            .await
            .map_err(|e| anyhow!(e))?;
        match response {
            ServiceResponse::Single(response) => match response {
                CoreResponse::GetThreadMessages(messages) => return Ok(messages),
                _ => (),
            },
            _ => (),
        };
        Err(anyhow!("unexpected response while getting thread messages"))
    }

    async fn maybe_start_listening_on_thread(&mut self) -> Result<()> {
        if self.listener_active {
            return Ok(());
        }
        if let Some(thread_id) = self.thread_id.clone() {
            let response = self
                .llmvm_core_service
                .call(CoreRequest::ListenOnThread(ListenOnThreadRequest {
                    thread_id,
                    client_id: self.client_id.clone(),
                }))
                .await
                .map_err(|e| anyhow!(e))?;
            let client_id = self.client_id.clone();
            tokio::spawn(async move {
                print!("\r");
                if let ServiceResponse::Multiple(mut stream) = response {
                    while let Some(result) = stream.next().await {
                        if let Ok(response) = result {
                            if let CoreResponse::ListenOnThread(message) = response {
                                if let Some(message) = message {
                                    let prefix =
                                        get_output_prefix(message.role, message.client_id.as_ref());
                                    print!(
                                        "\r{}{}\n{}",
                                        prefix,
                                        message.content,
                                        get_output_prefix(MessageRole::User, Some(&client_id))
                                    );
                                    stdout().lock().flush().unwrap();
                                }
                            }
                        }
                    }
                }
            });
            self.listener_active = true;
        }
        Ok(())
    }

    async fn handle_input(&mut self, line: String) -> Result<bool> {
        let trimmed_input = line.trim().to_string();
        if trimmed_input == "exit" {
            return Ok(false);
        }

        let response = self
            .llmvm_core_service
            .call(CoreRequest::GenerationStream(GenerationRequest {
                preset_id: self.cli.preset.clone(),
                parameters: self.cli.model.clone().map(|model| GenerationParameters {
                    model: Some(model),
                    prompt_parameters: Some(json!({ MESSAGE_PROMPT_PARAM: trimmed_input.clone() })),
                    ..Default::default()
                }),
                custom_prompt: Some(trimmed_input),
                existing_thread_id: self.thread_id.clone(),
                save_thread: true,
                client_id: Some(self.client_id.clone()),
            }))
            .await;

        match response {
            Err(e) => eprintln!("\nerror starting generation: {}", e),
            Ok(response) => {
                print!(
                    "{}",
                    get_output_prefix(MessageRole::Assistant, Some(&self.client_id))
                );
                self.stdout.flush().unwrap();
                match response {
                    ServiceResponse::Multiple(mut stream) => {
                        while let Some(result) = stream.next().await {
                            match result {
                                Err(e) => {
                                    eprintln!("\nerror during generation: {}", e);
                                    break;
                                }
                                Ok(response) => match response {
                                    CoreResponse::GenerationStream(response) => {
                                        if let Some(id) = response.thread_id {
                                            self.thread_id = Some(id);
                                            self.maybe_start_listening_on_thread().await?;
                                        }
                                        print!("{}", response.response);
                                        self.stdout.flush().unwrap();
                                    }
                                    _ => (),
                                },
                            }
                        }
                        println!();
                    }
                    _ => (),
                }
            }
        }
        Ok(true)
    }

    async fn read_input(&mut self) -> Result<bool> {
        match self
            .rl
            .readline(get_output_prefix(MessageRole::User, Some(&self.client_id)).as_str())
        {
            Ok(line) => self.handle_input(line).await,
            Err(e) => match e {
                ReadlineError::Interrupted | ReadlineError::Eof => Ok(false),
                _ => {
                    eprintln!("error reading line: {}", e);
                    Ok(true)
                }
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut chat = ChatApp::new().await?;

    chat.output_existing_messages().await?;
    chat.maybe_start_listening_on_thread().await?;

    loop {
        if !chat.read_input().await? {
            break;
        }
    }

    if let Some(thread_id) = chat.thread_id {
        eprintln!("\nThread ID is {}", thread_id);
    }

    Ok(())
}
