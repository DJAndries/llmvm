# llmvm-core-lib

[![Crates.io](https://img.shields.io/crates/v/llmvm-core-lib?style=for-the-badge)](https://crates.io/crates/llmvm-core-lib)
[![docs.rs](https://img.shields.io/docsrs/llmvm-core-lib?style=for-the-badge)](https://docs.rs/llmvm-core-lib)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

The library for the [llmvm](https://github.com/djandries/llmvm) core.

Assumes the following responsibilities:
- Sending generation requests to backends
- Managing message threads
- Model presets
- Prompt templates
- Projects / workspaces

See the executable wrapper [llmvm-core](https://github.com/djandries/llmvm/tree/master/core) for more information.

Frontends may optionally use this crate directly to avoid using any IPC communciation.

## Usage

Add the dependency to `Cargo.toml`:

```
[dependencies]
llmvm-core-lib = { version = "<version>" }
```

## Reference documentation

Reference documentation can be found on [docs.rs](https://docs.rs/llmvm-core-lib).

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)