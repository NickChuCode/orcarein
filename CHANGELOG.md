# Changelog

All notable changes to OrcaRein are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-02

First MVP milestone (`v0.1.0-mvp`). A small, hackable terminal agent.

### Added

- Streaming REPL agent harness over DeepSeek V4 and OpenAI-compatible providers
  (token-by-token output, multi-turn context).
- `Provider` trait with DeepSeek, OpenAI, and a `MockProvider` implementation,
  sharing one OpenAI-compatible SSE parser.
- Tool-use dispatch loop with a 5-tool suite: `read_file`, `write_file`,
  `list_dir`, `bash`, `edit` — each gated by a per-tool permission prompt.
- Session model with JSON persistence: save, list, and resume conversations;
  cumulative token-usage tracking.
- Configuration system: `config.toml` + a `SecretStore` (`secrets.toml`, `0600`
  on Unix); layered precedence (CLI flag > env var > TOML > default). API keys
  never touch `config.toml` or CLI flags.
- `orcarein doctor` offline health checks; quiet-by-default `tracing` diagnostics
  (opt in via `RUST_LOG`).
- `clap`-based CLI, `directories`-based cross-platform paths.

[Unreleased]: https://github.com/NickChuCode/orcarein/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/NickChuCode/orcarein/releases/tag/v0.1.0
