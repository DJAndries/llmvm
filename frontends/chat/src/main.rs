use std::{
    env,
    io::{stdout, Stdout, Write},
    process::exit,
};

use anyhow::{anyhow, Result};
use clap::Parser;
use futures::StreamExt;
use llmvm_protocol::{
    services::{
        service_with_timeout, util::build_core_service_from_config, BoxedService, ServiceResponse,
    },
    stdio::{CoreRequest, CoreResponse},
    tower::{timeout::Timeout, Service},
    GenerationParameters, GenerationRequest, HttpClientConfig, Message, MessageRole,
};
use llmvm_util::config::load_config;
use rustyline::{error::ReadlineError, Config as RlConfig, DefaultEditor as RlEditor, EditMode};
use serde::Deserialize;
use serde_json::json;

type CoreService = Timeout<BoxedService<CoreRequest, CoreResponse>>;

const CONFIG_FILENAME: &str = "chat.toml";
const MESSAGE_PROMPT_PARAM: &str = "user_message";

const NO_COLOR_ENV_VAR: &str = "NO_COLOR";
const USER_ROLE_PREFIX: &str = "\x1b[1;36myou > \x1b[0m";
const ASSISTANT_ROLE_PREFIX: &str = "\x1b[1;33massistant > \x1b[0m";
const USER_ROLE_NO_COLOR_PREFIX: &str = "you > ";
const ASSISTANT_ROLE_NO_COLOR_PREFIX: &str = "assistant > ";

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
pub struct ChatConfig {
    bin_path: Option<String>,

    http_core: Option<HttpClientConfig>,
}

struct ChatApp {
    cli: Cli,
    no_color: bool,
    rl: RlEditor,
    llmvm_core_service: CoreService,
    thread_id: Option<String>,
    stdout: Stdout,
}

impl ChatApp {
    async fn new() -> Result<Self> {
        let cli = Cli::parse();
        let no_color = !env::var(NO_COLOR_ENV_VAR).unwrap_or_default().is_empty();

        let mut config = load_config::<ChatConfig>(CONFIG_FILENAME).unwrap_or_else(|e| {
            eprintln!("failed to load config: {}", e);
            exit(1);
        });

        let llmvm_core_service: CoreService = service_with_timeout(
            build_core_service_from_config::<CoreRequest, CoreResponse>(
                config.bin_path.as_ref().map(|b| b.as_ref()),
                config.http_core.take(),
            )
            .await
            .map_err(|e| anyhow!(e))?,
        );

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
            no_color,
            rl,
            llmvm_core_service,
            thread_id: None,
            stdout,
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
                let prefix = self.get_output_prefix(message.role);
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

    fn get_output_prefix(&self, role: MessageRole) -> &'static str {
        match role {
            MessageRole::System => panic!("no output prefix for system role"),
            MessageRole::Assistant => match self.no_color {
                false => ASSISTANT_ROLE_PREFIX,
                true => ASSISTANT_ROLE_NO_COLOR_PREFIX,
            },
            MessageRole::User => match self.no_color {
                false => USER_ROLE_PREFIX,
                true => USER_ROLE_NO_COLOR_PREFIX,
            },
        }
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
            }))
            .await;

        match response {
            Err(e) => eprintln!("\nerror starting generation: {}", e),
            Ok(response) => {
                print!("{}", self.get_output_prefix(MessageRole::Assistant));
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
        match self.rl.readline(self.get_output_prefix(MessageRole::User)) {
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
