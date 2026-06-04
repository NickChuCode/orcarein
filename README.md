# OrcaRein

> An open-source Rust CLI agent harness for DeepSeek V4 and other OpenAI-compatible LLM providers. Inspired by [claw-code](https://github.com/ultraworkers/claw-code).

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
![Status: v0.1.0 MVP](https://img.shields.io/badge/status-v0.1.0--mvp-orange.svg)

OrcaRein is a small, hackable terminal agent: you chat with a model, and it can
read and modify files, run shell commands, and act on your project — gated by a
per-tool permission prompt. It streams responses token-by-token, remembers
multi-turn context, and persists sessions you can resume later.

> **Status — v0.1.0 MVP.** Usable, but pre-1.0: interfaces may change and the
> safety model is deliberately thin (see [Security](#security)). Use it on code
> you have backed up or under version control.

## Features

- **Streaming chat REPL** — token-by-token output, with the model's reasoning
  phase shown separately from its reply.
- **5 built-in tools** the model can call: `read_file`, `write_file`,
  `list_dir`, `bash` (cross-platform), `edit` (unique-match replace).
- **Per-tool permission prompt** — risky tools ask before running
  (`y`/`n`/`A`lways/`N`ever), with allow/deny decisions cached for the session.
  Deny-by-default.
- **Pluggable providers** — DeepSeek and OpenAI ship in-box behind one
  `Provider` trait; switch with `--provider` or `ORCAREIN_PROVIDER`.
- **Layered configuration** — `config.toml` + environment + CLI flags, resolved
  CLI > env > file > default. API keys live in the environment or a
  `0600` `secrets.toml`, never on the command line.
- **Session persistence** — every turn auto-saves to disk; `session list` /
  `session resume <id>` to pick up where you left off.
- **`doctor`** — an offline health check that tells you whether OrcaRein is
  configured correctly before you start.
- **Scriptable / non-interactive mode** — `--no-permission` (non-tty only),
  `--tools` allowlist, and `--system-prompt-file` for headless use.

## Install

Requires Rust **1.85+** (MSRV). Build from source:

```sh
git clone https://github.com/NickChuCode/orcarein
cd orcarein
cargo install --path crates/orcarein     # installs the `orcarein` binary
# — or just run it from the workspace:
cargo run --quiet
```

## Quickstart

```sh
# 1. Set your provider's API key (environment only — never a flag/file in the repo).
export DEEPSEEK_API_KEY="sk-..."          # PowerShell: $env:DEEPSEEK_API_KEY = "sk-..."

# 2. Check your setup.
orcarein doctor

# 3. Chat.
orcarein
```

Example session:

```
OrcaRein — chat with deepseek-v4-flash. /help for commands, Ctrl+D to quit.
Provider: deepseek
Tools: bash, edit, list_dir, read_file, write_file
> read Cargo.toml and tell me how many workspace members there are
[思考] ...
[tool: read_file({"path":"Cargo.toml"})]
[result] 612 bytes
[回复] This workspace has 2 members: orcarein-core and orcarein.
```

In the REPL, slash commands: `/help`, `/clear`, `/save`, `/usage`, `/exit`.

## Usage

```
orcarein [MODEL] [OPTIONS]          # start the chat REPL
orcarein doctor                     # offline health check
orcarein config get|set|list        # manage config.toml
orcarein session list|resume <id>   # manage saved sessions
```

| Flag | Meaning |
|---|---|
| `[MODEL]` | Provider-specific model id (overrides config + provider default) |
| `--provider <name>` | `deepseek` (default) or `openai` |
| `--tools <csv>` | Whitelist of tools, e.g. `read_file,list_dir` |
| `--no-permission` | Skip permission prompts — **non-tty stdin only** (a safety guard) |
| `--system-prompt-file <path>` | Read the system prompt from a file |

Environment: `DEEPSEEK_API_KEY` / `OPENAI_API_KEY`, `ORCAREIN_PROVIDER`,
`RUST_LOG` (e.g. `RUST_LOG=orcarein=debug` for diagnostics).

## Configuration

Resolved precedence: **CLI flag > environment variable > `config.toml` > built-in default.**

- `config.toml` — non-secret preferences (`provider`, `model`, `tools`,
  `system_prompt`). Manage with `orcarein config set provider openai`.
- `secrets.toml` — API keys, written `0600` on Unix. Keys are also read from the
  environment (env takes precedence). **Never** pass a key as a CLI flag.

Paths are per-platform (XDG on Linux, `%APPDATA%` on Windows); run
`orcarein doctor` to see the exact locations.

## Tools

| Tool | Risk | What it does |
|---|---|---|
| `read_file` | safe | Read a UTF-8 text file |
| `list_dir` | risky | List a directory (names can leak structure) |
| `write_file` | risky | Write/overwrite a file (parent dir must exist) |
| `edit` | risky | Replace a **unique** substring in a file |
| `bash` | risky | Run a shell command (`bash -c` / `cmd /C`) |

Risky tools trigger the permission prompt unless you cached an "always" decision
or passed `--no-permission`.

## Security

**OrcaRein is not a sandbox.** The permission prompt is the *only* safety layer
in v0.1, and it is deliberately thin:

- `bash`, `write_file`, and `edit` run with **your user's full privileges**.
  A model can delete or overwrite files, or run arbitrary commands, if you allow
  the call.
- The prompt is **deny-by-default** (empty input / EOF / anything unexpected →
  deny), but once you answer "always" for a tool, that tool runs unprompted for
  the rest of the session.
- `--no-permission` disables the prompt entirely. It is refused on an interactive
  terminal and only works with piped (non-tty) stdin — use it only in scripts you
  trust.

Run OrcaRein on code under version control or backed up. A real sandbox is a
post-1.0 goal.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The public contract lives in the tests (`crates/*/tests/` and `#[cfg(test)]`
modules). See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option.

## Acknowledgements

Architecture and tool design follow [claw-code](https://github.com/ultraworkers/claw-code).
