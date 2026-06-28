# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/NickChuCode/orcarein/compare/orcarein-core-v0.1.0...orcarein-core-v0.2.0) - 2026-06-28

### Added

- *(mention)* add expand_mentions for submit-time file injection
- *(mention)* add gitignore-aware list_project_files
- *(compact)* add compact_session orchestrator
- *(compact)* add render_span transcript flattening
- *(compact)* add Session::compact_at
- *(compact)* add compaction_boundary
- *(memory)* load + bound + format AGENTS.md block
- *(memory)* add AGENTS.md walk-up discovery
- *(tool)* add gitignore-aware search tool
- *(mcp)* add McpClient, McpTool, setup_servers + subprocess smoke test
- *(mcp)* add handshake/list/call protocol ops (duplex-tested)
- *(mcp)* add generic McpConnection transport (request/notify)
- *(mcp)* add McpServerConfig + Config.mcp_servers
- *(mcp)* add mcp feature scaffold + JSON-RPC/MCP wire types
- *(meter)* show context-window fill % in the per-turn meter and /usage
- *(session)* session delete <id> to prune saved sessions
- *(issue)* `orcarein issue <n>` — BYO-key self-bootstrap loop (E1)
- *(tool)* harden edit reliability — EOL tolerance, diagnostics, size guard
- *(tool)* tool-call argument repair + self-correcting errors
- *(cost)* split meter into input/output with an output-independent saved %
- *(cost)* cache-savings meter + economy/benchmark toggle
- *(agent)* headless `run` mode on an extracted agent engine

### Fixed

- *(tool)* cap bash stdout/stderr to prevent context overflow

### Other

- rustfmt /compact code
- *(example)* add repair_demo for tool-call argument repair

## [0.1.0](https://github.com/NickChuCode/orcarein/releases/tag/orcarein-core-v0.1.0) - 2026-06-05

### Fixed

- *(list_dir)* upgrade to Risky to prevent silent dir enumeration

### Other

- orcarein doctor health checks + tracing + bump to 0.1.0
- session persistence — JSON save/resume + session list/resume + auto-save
- config system — TOML + clap + layered precedence + SecretStore
- rename project DeepRig -> OrcaRein
