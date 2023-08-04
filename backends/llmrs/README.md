# llmvm-llmrs

[![Crates.io](https://img.shields.io/crates/v/llmvm-llmrs?style=for-the-badge)](https://crates.io/crates/llmvm-llmrs)
[![docs.rs](https://img.shields.io/docsrs/llmvm-llmrs?style=for-the-badge)](https://docs.rs/llmvm-llmrs)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

The library for the llmrs backend for [llmvm](https://github.com/djandries/llmvm).

Uses the [llm](https://github.com/rustformers/llm) crate to process text generation requests using local models. Supported models include LLaMA, GPT-2, GPT-J and more.

See the llm crate repository for more information on supported models.

## Installation

Install this backend using `cargo`.

```
cargo install llmvm-llmrs
```

## Usage

The backend can either be invoked directly, via [llmvm-core](https://github.com/djandries/llmvm/core) or via a frontend that utilizes llmvm-core.

To invoke directly, execute `llmvm-llmrs -h` for details.

## Configuration

Run the backend executable to generate a configuration file at `~/.config/llmvm/llmrs.toml`.

|Key|Required?|Description|
|--|--|--|
|`weights[].name`|Yes|Name of the model. This must match the `.bin` model file name in the llmvm weights directory, without the file extension.|
|`weights[].architecture`|Yes|Architecture of the model. i.e. llama, mpt, etc.|
|`weights[].context_tokens`|Yes|Context token count for the model.|
|`weights[].inference_session_config`|No|llm crate inference session configuration. [See this for details](https://docs.rs/llm/0.1.1/llm/struct.InferenceSessionConfig.html)|
|`tracing_directive`|No|Logging directive/level for [tracing](https://github.com/tokio-rs/tracing)|
|`stdio_server`|No|Stdio server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/protocol) for details.|
|`http_server`|No|HTTP server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/protocol) for details.|

Model weights must be stored in `~/.local/share/llmvm/weights` with the correct
model name, specified in your configuration.

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)