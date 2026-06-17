# Changelog

All notable changes to OrcaRein are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-06-17

First packaged release with prebuilt binaries for all platforms, including
**aarch64 Linux** for single-board computers (Raspberry Pi 4B/5, Orange Pi 5
Plus/5 Max).

### Added

- Headless `orcarein run "<task>"` — execute one task non-interactively and
  exit (scriptable / CI-friendly).
- `orcarein issue <number>` — fetch a GitHub issue and work it end-to-end
  (bring-your-own-key self-bootstrap loop).
- Cost/usage meter: per-turn token + cache-hit + spend line, context-window
  fill %, an `economy` cache mode, and `/usage` (= `/context`) on demand.
- Tool-call repair: malformed tool calls are corrected and surfaced as
  self-correcting errors instead of aborting the turn.
- Session management: interactive `session resume` picker, `session delete`,
  and unambiguous short-id prefix matching for resume/delete.
- Terminal UX: an alt-screen overlay + pager for `/show <file>` and `/history`,
  with `/` incremental search (n/N navigation, wrap-around, match highlighting);
  a GPIO live monitor (`orcarein hw monitor`, `hardware` feature). All
  capability-gated — degrades to plain output over SSH / serial / headless.
- Cross-SoC hardware layer (`orcarein-hardware`): board GPIO profiles for
  Raspberry Pi 4B/5 and Orange Pi 5 Plus/5 Max (pinouts transcribed from the
  official vendor docs), and a `gpiocdev`-based GPIO backend that sets/reads
  pins by logical name across Broadcom and Rockchip boards.

### Changed

- HTTP client now uses **rustls** instead of OpenSSL — portable
  cross-compilation to aarch64 and bundled roots (no system cert store needed
  on minimal SBC images).
- CI now lints and MSRV-checks the full feature matrix
  (default / `--all-features` / `--no-default-features`).
- Hardware GPIO backend migrated from the retired, Broadcom-only `rppal` to the
  SoC-agnostic Linux character device (`gpiocdev`).

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
