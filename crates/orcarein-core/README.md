# orcarein-core

Core library for [OrcaRein](https://github.com/NickChuCode/orcarein) — an
open-source Rust agent harness for DeepSeek V4 and OpenAI-compatible LLM
providers.

This crate holds the reusable building blocks: the `Message` / `Session`
conversation model, the `Tool` trait + registry and the built-in tool suite,
the permission gate, the `Provider` streaming abstraction (DeepSeek / OpenAI /
mock), the TOML config + secret store, and the `doctor` health checks.

The `orcarein` binary (the CLI you actually run) is built on top of this crate.
See the [main repository](https://github.com/NickChuCode/orcarein) for the full
project, README, and usage.

## License

Licensed under either of MIT or Apache-2.0 at your option.
