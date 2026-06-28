# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/NickChuCode/orcarein/compare/orcarein-v0.1.0...orcarein-v0.2.0) - 2026-06-28

### Added

- *(modal)* wire mention popup into the editor I/O loop
- *(modal)* mention accept produces the @path replacement edit
- *(modal)* mention trigger/teardown from buffer state
- *(modal)* mention popup state with subsequence filter
- *(modal)* add Tab key action (no-op until mention popup uses it)
- *(mention)* expand @path mentions in REPL submit path
- *(markdown)* per-block soft-wrap for code blocks with continuation marker
- *(markdown)* water-fill table column shrink instead of dropping columns
- *(markdown)* add fit_cell width-exact table cell helper
- *(pager)* render Markdown in /show and /history
- *(repl)* runtime /model, /sessions, /resume, /new + context warnings
- *(ui)* colorize modal input and pager per the design system
- *(ui)* redesign header and colorize streaming, permission, and help
- *(color)* add semantic ANSI palette with capability degradation
- *(modal)* mode-aware cursor shape (block in normal/visual, bar in insert)
- *(modal)* wire modal_readline into REPL with rustyline fallback
- *(modal)* raw-mode input loop + modal_readline (inline viewport)
- *(modal)* OSC52 clipboard with hand-rolled base64 + 64KiB cap
- *(modal)* pure render -> RenderView (spans/cursor/scroll/status)
- *(modal)* reducer apply() with Effect + history recall + commit semantics
- *(modal)* vim command parser (count/operator/motion)
- *(modal)* undo/redo snapshot stack
- *(modal)* visual mode (v/V) + selection ops
- *(modal)* delete/change/yank operators + linewise p/P
- *(modal)* mode switches + char-safe insert/backspace
- *(modal)* cursor motions (hjkl/0/$/^/gg/G/w/b/e)
- *(modal)* scaffold EditBuffer data model + invariants
- *(header)* orca mascot + DeepSeek-blue border
- *(header)* unify overlay surfaces with slim title bar
- *(header)* replace startup banner with unified header box
- *(header)* add slim_title_bar
- *(header)* add render_header (one-line/single/double column)
- *(header)* add status_chips, short_id, abbreviate_home
- *(header)* add disp_width + char-safe truncate_to_width
- *(compact)* add /compact REPL command
- *(init)* add /init agentic AGENTS.md generation
- *(init)* add /init precondition (exists/shadow/proceed)
- *(memory)* inject AGENTS.md into REPL/run/issue prompts
- *(tool)* add gitignore-aware search tool
- *(repl)* add /tools command listing built-in + MCP tools
- *(mcp)* wire MCP server registration into the binary (mcp feature, default on)
- *(pager)* highlight search matches in the body
- *(pager)* add `/` incremental search with n/N navigation
- *(meter)* show context-window fill % in the per-turn meter and /usage
- *(hw)* GPIO live monitor (orcarein hw monitor) on the overlay primitive
- *(tui)* alt-screen overlay pager for /show and /history
- *(session)* resolve resume/delete by unambiguous id prefix
- *(session)* session delete <id> to prune saved sessions
- *(session)* interactive picker for 'session resume' with no id
- *(issue)* `orcarein issue <n>` — BYO-key self-bootstrap loop (E1)
- *(cost)* cache-savings meter + economy/benchmark toggle
- *(agent)* headless `run` mode on an extracted agent engine

### Fixed

- *(markdown)* flush parent item text before nested list to fix merge
- *(markdown)* keep quote bar on list items inside blockquotes
- *(modal)* clear old inline viewport before resizing (stale status bar)
- *(modal)* clear inline viewport on exit so the frame doesn't collide with REPL output
- *(modal)* arrow keys move the cursor in insert mode
- *(modal)* offset inline cursor by viewport origin (was pinned to terminal top)
- *(modal)* charwise multiline paste must not embed a newline

### Other

- *(modal)* rustfmt mention module and editor loop
- *(markdown)* generalize wrap prefix to span slices
- *(deps)* add pulldown-cmark behind the tui feature
- *(modal)* drop module dead_code allow + redundant D/C methods
- *(overlay)* extract enter_raw shared by modal + pager
- rustfmt /compact code
- rustfmt project-memory + /init code
- *(release)* prepare v0.2.0 with aarch64 Linux binaries

## [0.1.0](https://github.com/NickChuCode/orcarein/releases/tag/orcarein-v0.1.0) - 2026-06-05

### Other

- orcarein doctor health checks + tracing + bump to 0.1.0
- session persistence — JSON save/resume + session list/resume + auto-save
- config system — TOML + clap + layered precedence + SecretStore
- rename project DeepRig -> OrcaRein
