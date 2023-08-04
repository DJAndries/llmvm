use std::io::{stdout, Write};
use std::{process::exit, sync::Arc};

use clap::{arg, command, Args, Parser, Subcommand};
use futures::stream::StreamExt;
use llmvm_core_lib::{LLMVMCore, LLMVMCoreConfig};
use llmvm_protocol::http::server::{HttpServer, HttpServerConfig};
use llmvm_protocol::service::CoreService;
use llmvm_protocol::stdio::server::{StdioServer, StdioServerConfig};
use llmvm_protocol::{ConfigExampleSnippet, Core, GenerationParameters, GenerationRequest};
use llmvm_util::config::load_config;
use llmvm_util::logging::setup_subscriber;
use serde::Deserialize;

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

#[derive(Deserialize)]
struct CliConfigContent {
    tracing_directive: Option<String>,
    stdio_server: Option<StdioServerConfig>,
    http_server: Option<HttpServerConfig>,

    #[serde(flatten)]
    lib_config: LLMVMCoreConfig,
}

impl ConfigExampleSnippet for CliConfigContent {
    fn config_example_snippet() -> String {
        format!(
            r#"# The logging directive (see tracing crate for details); i.e. info, debug, etc.
# tracing_directive = "info"

{}

# Stdio server configuration
# [stdio_server]
{}

# HTTP server configuration
# [http_server]
{}"#,
            LLMVMCoreConfig::config_example_snippet(),
            StdioServerConfig::config_example_snippet(),
            HttpServerConfig::config_example_snippet(),
        )
    }
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

    #[arg(long)]
    stream: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let config: CliConfigContent = match load_config(CONFIG_FILENAME) {
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

    let core = Arc::new(match LLMVMCore::new(config.lib_config).await {
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
                    custom_prompt_template: Some(args.prompt),
                    max_tokens: Some(args.max_tokens),
                    ..Default::default()
                }),
                existing_thread_id: args.existing_thread_id,
                save_thread: args.save_thread,
                ..Default::default()
            };
            match args.stream {
                false => match core.generate(request).await {
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
                },
                true => match core.generate_stream(request).await {
                    Err(e) => {
                        eprintln!("failed to generate: {}", e);
                        exit(1);
                    }
                    Ok(mut stream) => {
                        while let Some(result) = stream.next().await {
                            match result {
                                Err(e) => {
                                    println!();
                                    eprintln!("error during generation: {}", e);
                                    exit(1);
                                }
                                Ok(response) => {
                                    print!("{}", response.response);
                                    stdout().flush().ok();
                                    if let Some(id) = response.thread_id {
                                        eprintln!("\nThread ID is {}", id);
                                    }
                                }
                            }
                        }
                        println!();
                    }
                },
            }
        }
        CoreCommand::StdioServer => {
            StdioServer::<_, _, _>::new(
                CoreService::new(core),
                config.stdio_server.unwrap_or_default(),
            )
            .run()
            .await?;
        }
        CoreCommand::HttpServer(args) => {
            let mut config = config.http_server.unwrap_or_default();
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
