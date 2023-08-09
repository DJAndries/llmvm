# llmvm-llmrs

[![Crates.io](https://img.shields.io/crates/v/llmvm-llmrs?style=for-the-badge)](https://crates.io/crates/llmvm-llmrs)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

An [llmvm](https://github.com/djandries/llmvm) backend which uses
the [llm](https://github.com/rustformers/llm) crate to process text generation requests using local models.
Supported models include LLaMA, GPT-2, GPT-J and more.

See the llm crate repository for more information on supported models.

Example of an llmvm model ID for this backend: `llmrs/llmrs-text/mpt-7b-chat-q4_0-ggjt`

## Installation

Since the latest `llm` crate is unpublished on crates.io, you will need to clone this repo to install this backend.

Clone the repo, and install this backend using `cargo`.

```
git clone https://github.com/djandries/llmvm
cargo install --path ./llmvm/backends/llmrs
```

## Usage

The backend can either be invoked directly, via [llmvm-core](https://github.com/djandries/llmvm/tree/master/core) or via a frontend that utilizes llmvm-core.

To invoke directly, execute `llmvm-llmrs -h` for details.

`llmvm-llmrs http` can be invoked to create a HTTP server for remote clients.

## Configuration

Run the backend executable to generate a configuration file at:

- Linux: `~/.config/llmvm/llmrs.toml`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/llmrs.toml`
- Windows: `AppData\Roaming\djandries\llmvm\config\llmrs.toml`

|Key|Required?|Description|
|--|--|--|
|`weights[].name`|Yes|Name of the model. This must match the `.bin` model file name in the llmvm weights directory, without the file extension.|
|`weights[].architecture`|Yes|Architecture of the model. i.e. llama, mpt, etc.|
|`weights[].context_tokens`|Yes|Context token count for the model.|
|`weights[].inference_session_config`|No|llm crate inference session configuration. [See this for details](https://docs.rs/llm/0.1.1/llm/struct.InferenceSessionConfig.html)|
|`tracing_directive`|No|Logging directive/level for [tracing](https://github.com/tokio-rs/tracing)|
|`stdio_server`|No|Stdio server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#stdio-server-configuration) for details.|
|`http_server`|No|HTTP server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#http-server-configuration) for details.|

Model weights must be stored in the `weights` directory of your project, or global user data directory with the correct
model name, specified in your configuration.

The global weights directory is located at:

- Linux: `~/.local/share/llmvm/weights`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/weights`
- Windows: `AppData\Roaming\djandries\llmvm\data\weights`

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)