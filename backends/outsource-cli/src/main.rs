use clap::{arg, command, Parser};
use llmvm_backend_util::{get_request, print_response, InputMode};
use llmvm_outsource::generate;
use std::process::exit;

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long)]
    api_key: Option<String>,

    #[command(subcommand)]
    input_mode: InputMode,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let request = get_request(&cli.input_mode);

    match generate(request, cli.api_key).await {
        Err(e) => {
            eprintln!("Failed to produce response: {}", e);
            exit(1);
        }
        Ok(response) => print_response(response, &cli.input_mode),
    }
}
