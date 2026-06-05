# orcarein

[OrcaRein](https://github.com/NickChuCode/orcarein) — a small, hackable Rust CLI
agent harness for DeepSeek V4 and OpenAI-compatible LLM providers.

You chat with a model in your terminal and it can read/modify files, run shell
commands, and act on your project — each tool gated by a permission prompt. It
streams responses token-by-token, remembers multi-turn context, and persists
sessions you can resume.

## Install

```sh
cargo install orcarein
```

Or download a prebuilt binary from the
[releases page](https://github.com/NickChuCode/orcarein/releases/latest).

## Usage

```sh
export DEEPSEEK_API_KEY=...   # or configure via `orcarein config` / secrets.toml
orcarein                      # start the REPL
orcarein doctor               # offline health check
```

See the [main repository](https://github.com/NickChuCode/orcarein) for full
documentation, configuration, and the security model.

## License

Licensed under either of MIT or Apache-2.0 at your option.
