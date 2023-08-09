# llmvm-core

[![Crates.io](https://img.shields.io/crates/v/llmvm-core?style=for-the-badge)](https://crates.io/crates/llmvm-core)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

The core application for [llmvm](https://github.com/djandries/llmvm) which is responsible for managing the following:
 
- Sending generation requests to backends
- Managing message threads
- Model presets
- Prompt templates
- Projects/workspaces

## Installation

Install this application using `cargo`.

```
cargo install llmvm-core
```

## Usage

The core can either be invoked directly, or via a frontend that utilizes core.

To invoke directly, execute `llmvm-core -h` for details.

Rust frontends should use `build_core_service_from_config` from the [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol) crate to create a stdio or HTTP client, to communicate with the core.

`llmvm-core http-server` can be invoked to create a HTTP server for remote clients.

## Configuration

Run the core executable to generate a configuration file at:

- Linux: `~/.config/llmvm/core.toml`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/core.toml`
- Windows: `AppData\Roaming\djandries\llmvm\config\core.toml`

|Key|Required?|Description|
|--|--|--|
|`thread_ttl_secs`|No|The max time-to-live for threads in seconds. Threads with a last modified time older than the TTL will be removed.|
|`stdio_client`|No|Configuration for all backend stdio clients. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#stdio-client-configuration) for details.|
|`http_backends.<backend name>`|No|HTTP client configurations for remote backends. See [llmvm-protocol](https://github.com/djandries/llmvm/tree/master/protocol#http-client-configuration) for details.|

### Projects / workspaces

Projects can be created in given directory/workspace by running `llmvm-core init-project`. An `.llmvm` directory will be created in the current directory.

Inside the new directory, some subdirectories will be created for `presets`, `prompts`, `threads`, `logs`, `config` and `weights`.

Using projects can be useful for isolating the above resources for a given workspace.
All llmvm commands called within a workspace with a project will read/write to/from the `.llmvm` subdirectories.
The global directory for each resource type will be used as a fallback, if a specific resource does not exist.

### Prompt templates

[Handlebars](https://handlebarsjs.com/) templates can be saved and used for prompt generation.

Prompt templates are saved in the `prompts` directory of the current project directory, or global user data directory. The filename must end in `.hbs`.

The global prompt directory is located at:

- Linux: `~/.local/share/llmvm/prompts`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/prompts`
- Windows: `AppData\Roaming\djandries\llmvm\data\prompts`

#### System role prompts

The `system_role` block helper may be used to define a system role prompt. Here is an example:

```
{{#system_role}}
Do a good job.
{{/system_role}}
```

Anything outside of the block helper will be sent using the "user" role. 

#### Built-in templates / template examples

The core comes with some built-in templates. See the [prompts directory](https://github.com/DJAndries/llmvm/tree/master/core-lib/prompts) in this repo to see all built-in templates.

### Threads

System, user and assistant messages can be stored in threads. If the frontend sets `save_thread` to true in the generation request,
the prompt and model response will be saved to a new or existing thread. The generated thread ID will be provided in the response, via the `thread_id` key.
Existing threads may be used in generation requests, by specifying `existing_thread_id` in the request.

Threads are stored as JSON arrays in the `threads` directory of the current project directory, or global user data directory.

The global threads directory is located at:

- Linux: `~/.local/share/llmvm/threads`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/threads`
- Windows: `AppData\Roaming\djandries\llmvm\data\threads`

Old threads will be automatically deleted, if they were not updated recently. If the last modified age exceeds the thread TTL (14 days by default), the thread will be deleted.

### Presets

Presets contain generation parameters for core generation requests. The presets may contain prompt template ids, model ids,
model parameters, prompt parameters and more. Here are the keys for a preset:

|Key|Description|
|--|--|
|model|Model ID to use for generation.|
|prompt_template_id|ID for a saved prompt template.|
|custom_prompt_template|Text for a custom prompt template.|
|max_tokens|Maximum amount of tokens to generate.|
|model_parameters|Table of parameters for the model itself.|
|prompt_parameters|Table of parameters for completing the prompt template.|

Preset ids may be specified in generation requests, and preset settings may be overridden explicitly within the request.

Model presets are saved in the `presets` directory of the current project directory, or global user data directory. The filename must end in `.toml`.

The global preset directory is located at:

- Linux: `~/.local/share/llmvm/presets`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/presets`
- Windows: `AppData\Roaming\djandries\llmvm\data\presets`

#### Built-in presets / preset examples

The core comes with some built-in presets. See the [presets directory](https://github.com/DJAndries/llmvm/tree/master/core-lib/presets) in this repo to see all built-in presets.

### Logging

The crate uses the tracing library to handle logging. By default, all logging is written to stderr. If the `--log-to-file` switch is provided,
logs will be saved to the `logs` directory of the current project directory, or global user data directory.

The global logs directory is located at:

- Linux: `~/.local/share/llmvm/logs`.
- macOS: `~/Library/Application Support/com.djandries.llmvm/logs`
- Windows: `AppData\Roaming\djandries\llmvm\data\logs`

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)