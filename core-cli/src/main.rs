use std::{process::exit, sync::Arc};

use clap::{arg, command, Args, Parser, Subcommand};
use llmvm_core::{LLMVMCore, LLMVMCoreConfig};
use llmvm_protocol::http::HttpServer;
use llmvm_protocol::services::CoreService;
use llmvm_protocol::{stdio::StdioServer, Core, GenerationParameters, GenerationRequest};
use llmvm_util::config::load_config;
use llmvm_util::logging::setup_subscriber;

const CONFIG_FILENAME: &str = "core.toml";
const LOG_FILENAME: &str = "core.log";

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: CoreCommand,

    #[arg(long)]
    log_to_file: bool,
}

#[derive(Args, Clone)]
struct HttpServerArgs {
    #[arg(short, long)]
    port: Option<u16>,
}

#[derive(Subcommand)]
enum CoreCommand {
    Generate(GenerateArgs),
    StdioServer,
    HttpServer(HttpServerArgs),
    InitProject,
}

#[derive(Args, Clone)]
pub struct GenerateArgs {
    #[arg(long)]
    model: String,

    #[arg(long)]
    prompt: String,

    #[arg(long)]
    model_parameters_preset_id: Option<String>,

    #[arg(long)]
    existing_thread_id: Option<String>,

    #[arg(long)]
    save_thread: bool,

    #[arg(long)]
    max_tokens: u64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let mut config: LLMVMCoreConfig = match load_config(CONFIG_FILENAME) {
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

    let mut http_config = config.http_server.take();

    let core = Arc::new(match LLMVMCore::new(config).await {
        Ok(core) => core,
        Err(e) => {
            eprintln!("failed to init core: {}", e);
            exit(1);
        }
    });

    match cli.command {
        CoreCommand::Generate(args) => {
            let request = GenerationRequest {
                preset_id: None,
                parameters: Some(GenerationParameters {
                    model: Some(args.model),
                    prompt_template_id: None,
                    custom_prompt_template: Some(args.prompt),
                    max_tokens: Some(args.max_tokens),
                    model_parameters: None,
                    prompt_parameters: None,
                }),
                existing_thread_id: args.existing_thread_id,
                save_thread: args.save_thread,
            };
            match core.generate(request).await {
                Err(e) => {
                    eprintln!("failed to generate: {}", e);
                    exit(1);
                }
                Ok(response) => {
                    println!("{}", response.response);
                    if let Some(id) = response.thread_id {
                        eprintln!("Thread ID is {}", id);
                    }
                }
            }
        }
        CoreCommand::StdioServer => {
            StdioServer::new(CoreService::new(core)).run().await?;
        }
        CoreCommand::HttpServer(args) => {
            let mut config = http_config.take().unwrap_or_default();
            if let Some(port) = args.port {
                config.port = port;
            }
            if let Err(e) = HttpServer::new(CoreService::new(core), config).run().await {
                eprintln!("failed to start http server: {}", e);
                exit(1);
            }
        }
        CoreCommand::InitProject => match core.init_project() {
            Err(e) => {
                eprintln!("failed to generate: {}", e);
                exit(1);
            }
            Ok(_) => {
                eprintln!("initialized project in current folder");
            }
        },
    };
    Ok(())
}
