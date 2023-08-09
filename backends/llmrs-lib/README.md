# llmvm-llmrs-lib

[![Crates.io](https://img.shields.io/crates/v/llmvm-llmrs-lib?style=for-the-badge)](https://crates.io/crates/llmvm-llmrs-lib)
[![docs.rs](https://img.shields.io/docsrs/llmvm-llmrs-lib?style=for-the-badge)](https://docs.rs/llmvm-llmrs-lib)
[![GitHub](https://img.shields.io/github/license/djandries/llmvm?style=for-the-badge)](https://github.com/DJAndries/llmvm/blob/master/LICENSE)

The library for the llmrs backend for [llmvm](https://github.com/djandries/llmvm).

Uses the [llm](https://github.com/rustformers/llm) crate to process text generation requests using local models.

See the executable wrapper [llmvm-llmrs](https://github.com/djandries/llmvm/tree/master/backends/llmrs) for more information.

## Usage

Add the dependency to `Cargo.toml`:

```
[dependencies]
llmvm-llmrs-lib = { version = "<version>" }
```

## Reference documentation

Reference documentation can be found on [docs.rs](https://docs.rs/llmvm-llmrs-lib).

## License

[Mozilla Public License, version 2.0](https://spdx.org/licenses/MPL-2.0.html)