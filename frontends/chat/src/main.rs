use std::{
    env,
    io::{stdout, Stdout, Write},
    process::exit,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use clap::Parser;
use futures::StreamExt;
use llmvm_protocol::{
    http::client::HttpClientConfig,
    service::{util::build_core_service_from_config, BoxedService, CoreRequest, CoreResponse},
    stdio::client::StdioClientConfig,
    ConfigExampleSnippet, GenerationParameters, GenerationRequest, GetThreadMessagesRequest,
    Message, MessageRole, NewThreadInSessionRequest, ServiceResponse, SubscribeToThreadRequest,
    ThreadEvent,
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
const CONSOLE_ROLE_PREFIX: &str = "\x1b[1;32mconsole";
const CONSOLE_ROLE_NO_COLOR_PREFIX: &str = "console";
const RESET_COLOR: &str = "\x1b[0m";
const USER_ROLE_NO_COLOR_PREFIX: &str = "you";
const ASSISTANT_ROLE_NO_COLOR_PREFIX: &str = "assistant";

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Preset ID to use
    #[arg(short, long, default_value = "gpt-3.5-chat")]
    preset: Option<String>,

    /// Override model ID in preset
    #[arg(short, long)]
    model: Option<String>,

    /// Load last active chat thread
    #[arg(short, long)]
    load_last_chat: bool,

    /// Existing thread ID to load
    #[arg(short = 't', long)]
    load_thread_id: Option<String>,

    /// Tag within a session to use.
    /// This switch takes precedence over --load_thread_id
    #[arg(long)]
    tag: Option<String>,

    /// ID of the session to use.
    /// If a tag is provided, then this will default to the current directory path.
    #[arg(long)]
    session_id: Option<String>,

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
    in_console: Arc<Mutex<bool>>,
}

fn get_output_prefix(
    role: MessageRole,
    client_id: Option<&String>,
    in_console: &Mutex<bool>,
) -> String {
    let no_color = !env::var(NO_COLOR_ENV_VAR).unwrap_or_default().is_empty();
    let role_str = match role {
        MessageRole::System => panic!("no output prefix for system role"),
        MessageRole::Assistant => match no_color {
            false => ASSISTANT_ROLE_PREFIX,
            true => ASSISTANT_ROLE_NO_COLOR_PREFIX,
        },
        MessageRole::User => match *in_console.lock().unwrap() {
            true => match no_color {
                true => CONSOLE_ROLE_NO_COLOR_PREFIX,
                false => CONSOLE_ROLE_PREFIX,
            },
            false => match no_color {
                true => USER_ROLE_NO_COLOR_PREFIX,
                false => USER_ROLE_PREFIX,
            },
        },
    };
    match client_id {
        Some(client_id) => format!("{} ({}) > {}", role_str, client_id, RESET_COLOR),
        None => format!("{} > {}", role_str, RESET_COLOR),
    }
}

impl ChatApp {
    async fn new() -> Result<Self> {
        let mut cli = Cli::parse();

        if cli.session_id.is_none() && cli.tag.is_some() {
            cli.session_id = Some(std::env::current_dir()?.to_string_lossy().into_owned());
        }

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
            in_console: Default::default(),
        };
        if new.cli.tag.is_none() {
            new.thread_id = if new.cli.load_last_chat {
                new.get_last_thread_id().await?
            } else {
                new.cli.load_thread_id.clone()
            };
        }
        Ok(new)
    }

    async fn output_existing_messages(&mut self) -> Result<()> {
        if self.thread_id.is_some() || self.cli.tag.is_some() {
            for message in self.get_thread_messages().await? {
                let prefix = get_output_prefix(
                    message.role,
                    message.client_id.as_ref(),
                    self.in_console.as_ref(),
                );
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

    async fn get_thread_messages(&mut self) -> Result<Vec<Message>> {
        let response = self
            .llmvm_core_service
            .call(CoreRequest::GetThreadMessages(GetThreadMessagesRequest {
                thread_id: self.thread_id.clone(),
                session_id: self.cli.session_id.clone(),
                session_tag: self.cli.tag.clone(),
                ..Default::default()
            }))
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
        if self.thread_id.is_some() || self.cli.tag.is_some() {
            let thread_id = match self.cli.tag.is_some() {
                true => None,
                false => self.thread_id.clone(),
            };
            let response = self
                .llmvm_core_service
                .call(CoreRequest::SubscribeToThread(SubscribeToThreadRequest {
                    thread_id,
                    session_id: self.cli.session_id.clone(),
                    session_tag: self.cli.tag.clone(),
                    client_id: self.client_id.clone(),
                    ..Default::default()
                }))
                .await
                .map_err(|e| anyhow!(e))?;
            let client_id = self.client_id.clone();
            let in_console = self.in_console.clone();
            tokio::spawn(async move {
                print!("\r");
                if let ServiceResponse::Multiple(mut stream) = response {
                    while let Some(result) = stream.next().await {
                        if let Ok(response) = result {
                            if let CoreResponse::ListenOnThread(event) = response {
                                match event {
                                    ThreadEvent::Message { message } => {
                                        let prefix = get_output_prefix(
                                            message.role,
                                            message.client_id.as_ref(),
                                            &Mutex::new(false),
                                        );
                                        println!("\r{}{}", prefix, message.content,);
                                    }
                                    ThreadEvent::NewThread { thread_id } => {
                                        println!("\r\nNew thread started in group: {thread_id}\n");
                                    }
                                    _ => (),
                                }
                                print!(
                                    "\r{}",
                                    get_output_prefix(
                                        MessageRole::User,
                                        Some(&client_id),
                                        in_console.as_ref()
                                    ),
                                );
                                stdout().lock().flush().unwrap();
                            }
                        }
                    }
                }
            });
            self.listener_active = true;
        }
        Ok(())
    }

    fn enter_console(&mut self) {
        *self.in_console.lock().unwrap() = true;
        println!("\nAvailable commands:\nnew_group_thread - Creates a new thread within thread group, if one is being used\nret - Return to the chat\n");
    }

    async fn handle_console_input(&mut self, trimmed_input: &str) -> Result<()> {
        match trimmed_input {
            "new_group_thread" => {
                if self.cli.session_id.is_none() || self.cli.tag.is_none() {
                    return Ok(());
                }
                self.llmvm_core_service
                    .call(CoreRequest::NewThreadInSession(NewThreadInSessionRequest {
                        session_id: self.cli.session_id.clone().unwrap(),
                        tag: self.cli.tag.clone().unwrap(),
                    }))
                    .await
                    .map_err(|e| anyhow!(e))?;
            }
            "ret" => {
                *self.in_console.lock().unwrap() = false;
            }
            _ => (),
        }
        Ok(())
    }

    async fn handle_input(&mut self, line: String) -> Result<bool> {
        let trimmed_input = line.trim().to_string();
        if *self.in_console.lock().unwrap() {
            self.handle_console_input(&trimmed_input).await?;
            return Ok(true);
        }
        match trimmed_input.as_str() {
            "exit" => return Ok(false),
            "!c" => {
                self.enter_console();
                return Ok(true);
            }
            _ => (),
        }

        let thread_id = match self.cli.tag.is_some() {
            true => None,
            false => self.thread_id.clone(),
        };
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
                existing_thread_id: thread_id,
                session_id: self.cli.session_id.clone(),
                session_tag: self.cli.tag.clone(),
                save_thread: true,
                client_id: Some(self.client_id.clone()),
                ..Default::default()
            }))
            .await;

        match response {
            Err(e) => eprintln!("\nerror starting generation: {}", e),
            Ok(response) => {
                print!(
                    "{}",
                    get_output_prefix(
                        MessageRole::Assistant,
                        Some(&self.client_id),
                        self.in_console.as_ref()
                    )
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
        match self.rl.readline(
            get_output_prefix(MessageRole::User, Some(&self.client_id), &self.in_console).as_str(),
        ) {
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

    println!("Use !c to invoke the console\n");

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
