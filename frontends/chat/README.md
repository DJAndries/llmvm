# llmvm-chat

[![Crates.io](https://img.shields.io/crates/v/llmvm-chat?style=for-the-badge)](https://crates.io/crates/llmvm-chat)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

An [llmvm](https://github.com/djandries/llmvm) frontend that acts as a CLI chat interface.

## Demo

[![asciicast](https://asciinema.org/a/601451.svg)](https://asciinema.org/a/601451)

## Installation

Install this application using `cargo`.

```
cargo install llmvm-chat
```

The llmvm core must be installed. If you have not done so, the core may be installed via
```
cargo install llmvm-core
```

A backend must be installed and configured. The [llmvm-outsource](https://github.com/DJAndries/llmvm/tree/master/backends/outsource) is recommended for OpenAI requests.
Currently, the default model preset is `gpt-3.5-chat` which uses this backend.

## Usage

Run `llmvm-chat` to use the interface. Press CTRL-C when you are finished with your chat. A chat thread will be persisted, and the thread ID will be outputted.

Use the `-h` to see all options.

Use the `-l` to load the last chat thread.

## Configuration

Run the chat executable to generate a configuration file at:

- Linux: `~/.config/llmvm/chat.toml`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/chat.toml`
- Windows: `AppData\Roaming\djandries\llmvm\config\chat.toml`

|Key|Required?|Description|
|--|--|--|
|`stdio_core`|No|Stdio client configuration for communicated with llmvm core. See [llmvm-protocol](https://github.com/DJAndries/llmvm/tree/master/protocol#stdio-client-configuration) for details.|
|`http_core`|No|HTTP client configuration for communicating with llmvm core. See [llmvm-protocol](https://github.com/DJAndries/llmvm/tree/master/protocol#http-client-configuration) for details.|

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)