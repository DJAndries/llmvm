use std::{
    io::{stdin, stdout, BufRead, Write},
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
    GenerationParameters, GenerationRequest, HttpClientConfig,
};
use llmvm_util::config::load_config;
use serde::Deserialize;
use serde_json::json;

const CONFIG_FILENAME: &str = "chat.toml";
const MESSAGE_PROMPT_PARAM: &str = "user_message";

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
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    bin_path: Option<String>,

    http_core: Option<HttpClientConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut config = load_config::<ChatConfig>(CONFIG_FILENAME).unwrap_or_else(|e| {
        eprintln!("failed to load config: {}", e);
        exit(1);
    });

    let mut llmvm_core_service: Timeout<BoxedService<_, _>> = service_with_timeout(
        build_core_service_from_config::<CoreRequest, CoreResponse>(
            config.bin_path.as_ref().map(|b| b.as_ref()),
            config.http_core.take(),
        )
        .await
        .map_err(|e| anyhow!(e))?,
    );

    let mut thread_id = cli.load_thread_id.clone();

    let stdin = stdin();
    let mut stdout = stdout();
    let mut input = String::new();

    loop {
        print!("you > ");
        stdout.flush().unwrap();

        stdin.lock().read_line(&mut input).unwrap();

        if input.trim() == "exit" {
            break;
        }

        let response = llmvm_core_service
            .call(CoreRequest::GenerationStream(GenerationRequest {
                preset_id: cli.preset.clone(),
                parameters: cli.model.clone().map(|model| GenerationParameters {
                    model: Some(model),
                    prompt_parameters: Some(json!({ MESSAGE_PROMPT_PARAM: input.clone() })),
                    ..Default::default()
                }),
                custom_prompt: Some(input.clone()),
                existing_thread_id: thread_id.clone(),
                save_thread: true,
            }))
            .await;

        match response {
            Err(e) => eprintln!("\nerror starting generation: {}", e),
            Ok(response) => {
                print!("assistant > ");
                stdout.flush().unwrap();
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
                                            thread_id = Some(id);
                                        }
                                        print!("{}", response.response);
                                        stdout.flush().unwrap();
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

        input.clear();
    }

    if let Some(thread_id) = thread_id {
        eprintln!("\nThread ID is {}", thread_id);
    }

    Ok(())
}
