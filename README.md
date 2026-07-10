<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo_dark.png">
    <img src="assets/logo_light.png" alt="OrcaRein" width="280">
  </picture>

  <p><em>An open-source Rust CLI agent harness for DeepSeek V4 and any OpenAI-compatible LLM.</em></p>

  <p>
    <a href="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml"><img src="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
    <a href="https://github.com/NickChuCode/orcarein/releases/latest"><img src="https://img.shields.io/github/v/release/NickChuCode/orcarein?sort=semver" alt="Latest release"></a>
  </p>

  <p><strong>English</strong> · <a href="README.zh-CN.md">简体中文</a></p>
</div>

**OrcaRein** is a small, hackable terminal agent. You chat with a model and it
acts on your project — reading and searching files, editing code, running shell
commands, calling out to external tools — with every risky action gated by a
per-tool permission prompt. It streams responses token-by-token, keeps
multi-turn context, and persists sessions you can resume later.

It ships as a single static binary (DeepSeek by default, or any
OpenAI-compatible endpoint), with an embeddable `orcarein-core` library
underneath. It runs happily behind restricted networks with your own API key,
and degrades honestly: a rich TUI on capable terminals, clean plain output over
pipes, CI, and headless SBCs.

> **Status — v0.3.0.** Usable day-to-day, but pre-1.0: interfaces may still
> change, and there is **no sandbox yet** — the permission prompt is the only
> safety layer (see [Security](#security)). Run it on code under version
> control or backed up. A real sandbox is on the [roadmap](#roadmap).

---

## Capabilities

### Agent core

- **Streaming chat REPL** — token-by-token output, with the model's reasoning
  phase shown separately from its reply.
- **Multi-turn tool-calling loop** with automatic **tool-call repair**
  (malformed calls are recovered instead of crashing the turn) and a
  self-correcting error protocol.
- **Cost & token metering** — live usage accounting per session (`/usage`).

### Built-in tools

Eight tools the model can call, each with a declared risk level:

| Tool | Risk | What it does |
|---|---|---|
| `read_file` | safe | Read a UTF-8 text file |
| `search` | risky | Regex search across the tree, **gitignore-aware**, returns `path:line:text` |
| `list_dir` | risky | List a directory |
| `write_file` | risky | Write / overwrite a file |
| `edit` | risky | Replace a **unique** substring in a file |
| `bash` | risky | Run a shell command (`bash -c` / `cmd /C`), output capped at 32 KiB |
| `skill` | safe | Load a named instruction pack on demand (see [Skills](#skills--subagents)) |
| `task` | safe | Delegate a sub-task to an isolated sub-agent (see [Subagents](#skills--subagents)) |

Plus any **MCP-server tools** registered dynamically from your config.

### Permissions & safety

- **Per-tool permission prompt** — risky tools ask before running
  (`y` / `n` / `A`lways / `N`ever). **Deny-by-default**: empty input, EOF, or
  anything unexpected denies the call.
- Allow / deny decisions are **cached for the session**; answer "always" once
  and that tool runs unprompted for the rest of the run.

### Providers & models

- **Pluggable providers** — DeepSeek (default) and OpenAI ship in-box behind one
  `Provider` trait; switch with `--provider` or `ORCAREIN_PROVIDER`. Any
  OpenAI-compatible endpoint works.
- **rustls TLS** — no OpenSSL, so cross-compiling to aarch64 Linux (Raspberry
  Pi / Orange Pi) is painless and the binary carries bundled roots.
- **Transient-failure retry** — 429 / 5xx / network blips are retried with
  exponential backoff before the stream starts; configurable via a `[retry]`
  config section.
- **Runtime model switching** — `/model` switches models live and persists the
  choice; an in-editor picker lists models fetched from the provider's
  `GET /v1/models`, so new models appear and retired ones disappear
  automatically.

### Sessions & context

- **Session persistence** — every turn auto-saves to disk; `session list` /
  `session resume <id>` / `session delete <id>`, and runtime `/sessions`,
  `/resume`, `/new`.
- **Manual context compaction** — `/compact` flattens and summarizes the
  conversation to reclaim the context window (with an honest note about the
  cache cost).
- **Project memory** — a repo's `AGENTS.md` (walked up from the cwd) is folded
  into the system prompt; `/init` scaffolds one.

### Skills & subagents

- **Skills** — named, on-demand instruction packs discovered from
  `.orcarein/skills/*.md` (or `<name>/SKILL.md`). Only a lightweight index sits
  in the prompt; the model pulls a skill's full body via the `skill` tool when
  it's relevant. Browse them with `/skills`.
- **Subagents** — the `task` tool delegates a self-contained sub-task to a fresh
  sub-agent with its **own isolated context window**; only the concise result
  comes back, keeping the main conversation clean.
- **MCP client** — a hand-rolled Model Context Protocol stdio client (default-on
  `mcp` feature) registers external tools from a `[[mcp_servers]]` config block
  at startup.

### Terminal experience

- **Vim modal editor** — a self-built multiline input (normal / insert / visual
  modes, motions, operators, counts, undo/redo, OSC52 clipboard) that degrades
  back to a plain line editor when the terminal can't support it.
- **`@`-mention completion** — an in-editor popup completes project files and
  directories (gitignore-aware); at submit, `@path` injects the file content or
  directory tree so the model sees it without polluting the cached prefix.
- **Markdown pager with syntax highlighting** — `/show` and `/history` render
  headings, bold/italic, fenced code blocks, block quotes, lists, links, and
  CJK-aware tables, with a hand-rolled zero-dependency code highlighter.
- **Semantic color system** — 8 named tokens degrade truecolor → 256 → 16 →
  `NO_COLOR`, with a unified header showing model / cwd / session.
- **Honest degradation** — non-tty, `TERM=dumb`, serial, and headless
  environments fall back to clean plain printing.

### Headless & scripting

- **`run`** — execute a single task non-interactively (prompt as an argument or
  from stdin); the answer goes to stdout, diagnostics to stderr.
- **`--no-permission`** (non-tty stdin only), **`--tools`** allowlist, and
  **`--system-prompt-file`** for unattended use.
- **`issue <N>`** — a BYO-key self-bootstrap loop: read a GitHub issue, let the
  agent edit the code (read/list/edit/write, no shell), run `cargo test`, and
  show you the diff. It never commits, pushes, or opens a PR — that's your call.
- **`doctor`** — an offline health check that tells you whether OrcaRein is
  configured correctly before you start.

### Engineering

360+ tests, a three-platform CI matrix (Linux / macOS / Windows) at MSRV
**1.85**, Conventional Commits, `release-plz`-managed changelog, and a
tag-driven pipeline that ships **6 target binaries** per release.

---

## Roadmap

> 🚧 **In development — planned, not yet shipped.** These are targets, not
> commitments; dates and scope may shift. Track progress on the
> [releases](https://github.com/NickChuCode/orcarein/releases) page.

| Capability | What it adds | Target |
|---|---|---|
| **Permission v2** | `allow` / `ask` / `deny` rules (command + path patterns), persisted config, permission modes (`default` / `acceptEdits` / `plan` / `yolo`), path-risk escalation (`~/.ssh`, `.env`, `*.pem`) | v0.4.0 |
| **Hooks** | `PreToolUse` / `PostToolUse` lifecycle hooks — guardrails enforced by exit code, not prompt requests | v0.4.0 |
| **Auto-compact + tool-output pruning** | Threshold-triggered compaction with a reactive `prompt_too_long` fallback; non-destructive pruning of stale tool output; prefix-preserving so the cache survives | v0.5.0 |
| **Sandbox** | New `orcarein-sandbox` crate — Linux Landlock + seccomp (workspace-write + no-network by default), macOS Seatbelt, honest Windows degradation | v0.6.0 |
| **MCP over HTTP** | Streamable HTTP transport, `.mcp.json` project config, deferred tool schemas (idle MCP tools don't bloat the prefix); Playwright-MCP web browsing as the acceptance target | v0.7.0 |
| **Custom slash commands** | `.orcarein/commands/*.md`, reusing the skill discovery/parsing path | v0.7.0 |
| **Parallel subagents** | Fan-out `task` execution + custom agent types (model / tool allowlist / persona via Markdown frontmatter) | v0.7.0 |
| **Repomap** | tree-sitter symbol index for whole-repo code intelligence, offline | v0.7.0 |
| **Checkpoints & rollback** | Side-git snapshots + `/restore` to undo a run's edits | v0.8.0 |
| **Streaming Markdown** | Progressive Markdown rendering during token streaming | v0.8.0 |
| **Web tools** | `web_fetch` + `web_search` behind a `SearchBackend` trait (Bocha for CN, Tavily for intl) — usable behind restricted networks | v0.8.0 |
| **Plan mode / todo tool** | Lightweight planning + task-tracking surface | v0.8.0 |
| **Wider distribution** | crates.io resume, npm / brew wrappers, ecosystem-list inclusion | v0.9.0 |

A **1.0** is gated (not dated) on a frozen, audited `orcarein-core` public API.

---

## Install

### Prebuilt binaries (recommended)

Download the archive for your platform from the
[latest release](https://github.com/NickChuCode/orcarein/releases/latest)
(Linux x86_64 / aarch64, macOS x86_64 / arm64, Windows x86_64), extract it, and
put the `orcarein` binary somewhere on your `PATH`.

### From source

Requires Rust **1.85+** (MSRV):

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
Tools: bash, edit, list_dir, read_file, search, write_file
> how many workspace members does this repo have?
[思考] I'll read the workspace manifest.
[tool: read_file({"path":"Cargo.toml"})]
[result] 641 bytes
[回复] Four: orcarein-core, orcarein, orcarein-hardware, and orcarein-eval.
```

## Usage

```
orcarein [MODEL] [OPTIONS]          # start the chat REPL
orcarein run [PROMPT]               # run one task non-interactively (headless)
orcarein issue <N>                  # fix GitHub issue #N in the current repo
orcarein doctor                     # offline health check
orcarein config get|set|list        # manage config.toml
orcarein session list|resume|delete  # manage saved sessions
```

| Flag | Meaning |
|---|---|
| `[MODEL]` | Provider-specific model id (overrides config + provider default) |
| `--provider <name>` | `deepseek` (default) or `openai` |
| `--tools <csv>` | Whitelist of tools, e.g. `read_file,search` |
| `--no-permission` | Skip permission prompts — **non-tty stdin only** (a safety guard) |
| `--system-prompt-file <path>` | Read the system prompt from a file |

**Slash commands** (in the REPL): `/help`, `/clear`, `/model`, `/tools`,
`/skills`, `/compact`, `/usage`, `/save`, `/show`, `/history`, `/init`,
`/sessions`, `/resume`, `/new`, `/exit`.

Environment: `DEEPSEEK_API_KEY` / `OPENAI_API_KEY`, `ORCAREIN_PROVIDER`,
`RUST_LOG` (e.g. `RUST_LOG=orcarein=debug` for diagnostics).

## Configuration

Resolved precedence: **CLI flag > environment variable > `config.toml` > built-in default.**

- `config.toml` — non-secret preferences (`provider`, `model`, `tools`,
  `system_prompt`, `[retry]`, `[[mcp_servers]]`). Manage with
  `orcarein config set provider openai`.
- `secrets.toml` — API keys, written `0600` on Unix. Keys are also read from the
  environment (env takes precedence). **Never** pass a key as a CLI flag.

Paths are per-platform (XDG on Linux, `%APPDATA%` on Windows); run
`orcarein doctor` to see the exact locations.

## Security

**OrcaRein is not a sandbox** (yet — see the [roadmap](#roadmap)). The permission
prompt is currently the *only* safety layer, and it is deliberately thin:

- `bash`, `write_file`, and `edit` run with **your user's full privileges**. A
  model can delete or overwrite files, or run arbitrary commands, if you allow
  the call.
- The prompt is **deny-by-default**, but once you answer "always" for a tool,
  that tool runs unprompted for the rest of the session.
- `--no-permission` disables the prompt entirely. It is refused on an
  interactive terminal and only works with piped (non-tty) stdin — use it only
  in scripts you trust.

Run OrcaRein on code under version control or backed up.

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

Early architecture and tool design were informed by studying
[claw-code](https://github.com/ultraworkers/claw-code).
