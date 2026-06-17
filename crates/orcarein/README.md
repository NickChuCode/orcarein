# orcarein

[OrcaRein](https://github.com/NickChuCode/orcarein) — a small, hackable Rust CLI
agent harness for DeepSeek V4 and OpenAI-compatible LLM providers.

You chat with a model in your terminal and it can read/modify files, run shell
commands, and act on your project — each tool gated by a permission prompt. It
streams responses token-by-token, remembers multi-turn context, and persists
sessions you can resume.

## Install

**Prebuilt binary (recommended)** — grab the archive for your platform from the
[releases page](https://github.com/NickChuCode/orcarein/releases/latest).
Single-board computers (Raspberry Pi 4B/5, Orange Pi 5 Plus/5 Max) use an
**aarch64 Linux** build:

```sh
# On the board (aarch64 Linux). Pick the gnu archive; if it complains about the
# glibc version, use the `-musl` archive instead (fully static, runs anywhere).
ver=v0.2.0
curl -LO https://github.com/NickChuCode/orcarein/releases/download/$ver/orcarein-$ver-aarch64-unknown-linux-gnu.tar.gz
tar xzf orcarein-$ver-aarch64-unknown-linux-gnu.tar.gz
sudo install orcarein /usr/local/bin/
orcarein --version
```

x86_64 Linux, macOS (Intel/Apple Silicon), and Windows archives are on the same
page. From source: `cargo install orcarein`.

## Configure your API key

The key is read from an environment variable first, then `secrets.toml`. It
never goes into `config.toml` or a CLI flag.

```sh
# Option A — environment variable (simplest):
export DEEPSEEK_API_KEY=sk-...        # add to ~/.bashrc to persist

# Option B — secrets.toml (persisted, 0600 on Unix). Run `orcarein doctor` to
# see the exact path, then create it (Linux default ~/.config/orcarein/):
#   [keys]
#   deepseek = "sk-..."
```

`orcarein doctor` prints a PASS/WARN/FAIL report including whether the key was
found and where the config/secrets files live.

## Usage

```sh
orcarein                          # interactive REPL (chat + tools)
orcarein run "summarize README"   # one-shot headless task, then exit
orcarein doctor                   # offline health check
orcarein issue 42                 # fetch GitHub issue #42 and work it
orcarein session list             # list saved conversations
orcarein session resume           # pick a session to resume (or: resume <id>)
```

Inside the REPL, slash commands: `/help`, `/show <file>` and `/history`
(pager — `/` to search, `n`/`N` to navigate), `/usage` (token + cost + context
fill %), `/save`, `/clear`. Each file/shell tool asks permission before running.

See the [main repository](https://github.com/NickChuCode/orcarein) for full
documentation, configuration, and the security model.

## License

Licensed under either of MIT or Apache-2.0 at your option.
