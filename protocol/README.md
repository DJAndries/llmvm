# llmvm-protocol

[![Crates.io](https://img.shields.io/crates/v/llmvm-protocol?style=for-the-badge)](https://crates.io/crates/llmvm-protocol)
[![docs.rs](https://img.shields.io/docsrs/llmvm-protocol?style=for-the-badge)](https://docs.rs/llmvm-protocol)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

A library that contains protocol/communication elements for [llmvm](https://github.com/djandries/llmvm).

Uses [multilink](https://github.com/djandries/multilink) for stdio/local process and HTTP/remote process communication.

Contains:

- Request/response types for core and backend communication
- Traits for core and backend implementations
- [multilink](https://github.com/djandries/multilink) / [tower](https://github.com/tower-rs/tower) services
- JSON-RPC and HTTP request/response conversion trait implementations for [multilink](https://github.com/djandries/multilink).

All llmvm backends should implement the `Backend` trait, and use the [llmvm-backend-util](https://github.com/djandries/llmvm/tree/master/backends/util) crate to start a stdio or HTTP server.

Rust frontends should use `util::build_core_service_from_config` in this crate to create a stdio or HTTP client, to communicate with the core.

## Usage

Add the dependency to `Cargo.toml`:

```
[dependencies]
llmvm-protocol = { version = "<version>", features = ["<http-server|http-client|stdio-server|stdio-client>"] }
```

## Configuration

### stdio client configuration

Core and frontends will typically expose optional stdio client configuration. Here are the keys for the configuration structure:

|Key|Required?|Description|
|--|--|--|
|`bin_path`|No|Optional binary path for spawning child processes. Defaults to PATH.|
|`timeout_secs`|No|Timeout for client requests in seconds.|

### stdio server configuration

Core and backends will typically expose optional stdio server configuration. Here are the keys for the configuration structure:

|Key|Required?|Description|
|--|--|--|
|`service_timeout_secs`|No|Timeout for service requests in seconds.|

### HTTP client configuration

Core and frontends will typically expose optional HTTP client configuration. Here are the keys for the configuration structure:

|Key|Required?|Description|
|--|--|--|
|`base_url`|Yes|Base URL/prefix for all outgoing requests.|
|`api_key`|If llmvm server has API key set|HTTP server API key to append to llmvm requests. The key will be inserted into the `X-API-Key` header.|
|`timeout_secs`|No|Timeout for client requests in seconds.|

### HTTP server configuration

Core and backends will typically expose optional HTTP server configuration. Here are the keys for the configuration structure:

|Key|Required?|Description|
|--|--|--|
|`port`|No|Port to listen on. Defaults to 8080.|
|`api_keys[]`|No|An optional set of permitted API keys for restricting access to the server. If omitted, an API key is not needed to make a request.|
|`service_timeout_secs`|No|Timeout for service requests in seconds.|

## Reference documentation

Reference documentation can be found on [docs.rs](https://docs.rs/llmvm-protocol).

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)
