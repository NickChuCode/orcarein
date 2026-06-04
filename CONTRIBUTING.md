# Contributing to OrcaRein

Thanks for your interest! OrcaRein is small and hackable by design — getting a
change in should be quick.

## Getting started

```sh
git clone https://github.com/NickChuCode/orcarein
cd orcarein
cargo build
cargo test --workspace
```

Requires Rust **1.85+** (the MSRV declared in `Cargo.toml`). Develop on current
stable.

## Before you open a PR

Run the same three checks CI runs (see Chapter 18) — all must pass:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## How the codebase is organized

Two crates in one workspace:

- **`orcarein-core`** — the library: `Message`, `Session`, the `Tool` and
  `Provider` traits and their implementations, config, permissions, doctor.
  Errors here are concrete (`thiserror`-derived) so callers can match on them.
- **`orcarein`** — the binary: CLI parsing, the REPL, the tool-dispatch loop,
  and the permission prompt. Uses `anyhow` for error propagation.

Rule of thumb: **decision logic and reusable types go in `orcarein-core`;
I/O and user interaction go in the binary.** This keeps the core unit-testable
without mocking the world (see `doctor.rs` for the pattern: pure verdict
functions in the lib, fact-gathering in the bin).

## The contract is the tests

OrcaRein's behavior is pinned by its tests:

- Unit tests in `#[cfg(test)] mod tests` blocks next to the code.
- Integration tests in `crates/orcarein-core/tests/` that use only the public
  API (and a `MockProvider` for the full tool loop, no network).

**If you change behavior, change or add a test.** A PR that alters what a tool
returns or how a message serializes, without touching a test, will be asked to
add one. Read the existing tests to learn the expected contract before changing
anything.

## Conventions

- Comments and identifiers in English.
- Prefer `&str`/`&[T]` parameters; reach for `clone()` only when ownership is
  genuinely needed.
- Don't introduce a trait with a single implementation, or a dependency that
  isn't pulled by a concrete need.
- Tool errors are inputs to the model: when a tool fails, return an actionable
  message (what went wrong + how to fix), not a bare error.

## Adding a tool

Each tool is one file under `crates/orcarein-core/src/tool/<name>.rs`
implementing the `Tool` trait, registered in the binary. Add a metadata unit
test and an integration test under `tests/<name>_tool.rs`. Pick a `RiskLevel`:
`Safe` for read-only, `Risky` for anything that touches the filesystem or shell.

## A note on safety

OrcaRein is not sandboxed (see the README's Security section). Changes that
broaden what a tool can do without a corresponding permission/`RiskLevel`
consideration will get extra scrutiny.

## Licensing

By contributing, you agree that your contributions are dual-licensed under
[MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE), matching the project.
