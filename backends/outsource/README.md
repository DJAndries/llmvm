# llmvm-outsource

[![Crates.io](https://img.shields.io/crates/v/llmvm-outsource?style=for-the-badge)](https://crates.io/crates/llmvm-outsource)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

An [llmvm](https://github.com/djandries/llmvm) backend which sends text and chat 
generation requests to known hosted language model providers.

Supported providers:

- OpenAI (text & chat interface)
- Anthropic (chat interface)
- Hugging Face (text interface)
- Ollama (text interface)

Example of an llmvm model ID for this backend: `outsource/openai-chat/gpt-3.5-turbo`

## Installation

Install this backend using `cargo`.

```
cargo install llmvm-outsource
```

## Usage

The backend can either be invoked directly, via [llmvm-core](https://github.com/djandries/llmvm/tree/master/core) or via a frontend that utilizes llmvm-core.

To invoke directly, execute `llmvm-outsource -h` for details.

`llmvm-outsource http` can be invoked to create a HTTP server for remote clients.

## Configuration

Run the backend executable to generate a configuration file at:

- Linux: `~/.config/llmvm/outsource.toml`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/outsource.toml`
- Windows: `AppData\Roaming\djandries\llmvm\config\outsource.toml`

|Key|Required?|Description|
|--|--|--|
|`openai_api_key`|If using OpenAI|API key for OpenAI requests.|
|`huggingface_api_key`|If using Hugging Face|API key for Hugging Face requests.|
|`anthropic_api_key`|If using Anthropic|API key for Anthropic requests.|
|`ollama_endpoint`|If using Ollama|Endpoint for Ollama requests (defaults to `http://127.0.0.1:11434/api/generate`)|
|`openai_endpoint`|If using a custom OpenAI server|Custom endpoint for all OpenAI requests. Supports any OpenAI API compatible server (i.e. fastchat).|
|`tracing_directive`|No|Logging directive/level for [tracing](https://github.com/tokio-rs/tracing)|
|`stdio_server`|No|Stdio server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#stdio-server-configuration) for details.|
|`http_server`|No|HTTP server settings. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#http-server-configuration) for details.|

### Hugging Face and OpenAI custom endpoints

OpenAI custom endpoints may be specified via the config file (recommended, see above), or via the model ID at runtime.
HuggingFace custom endpoints may only be specified via the model ID.

Within the model ID, custom hosted endpoints may be specified by supplying the prefix `endpoint=`, followed by the endpoint
URL.

For OpenAI, add the endpoint after the model name. For example, the model ID could be `outsource/openai-chat/vicuna-7b-v1.5/endpoint=https://localhost:8000`.

For HuggingFace, replace the model name with the endpoint. For example, the model ID could be `outsource/huggingface-text/endpoint=https://yourendpointhere`.

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)
