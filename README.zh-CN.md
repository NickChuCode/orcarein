<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo_dark.png">
    <img src="assets/logo_light.png" alt="OrcaRein" width="280">
  </picture>

  <p><em>一个开源的 Rust CLI agent harness，面向 DeepSeek V4 及 OpenAI 兼容的大模型。</em></p>

  <p>
    <a href="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml"><img src="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#许可证"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
    <img src="https://img.shields.io/badge/status-v0.1.0--mvp-orange.svg" alt="Status: v0.1.0 MVP">
  </p>

  <p><a href="README.md">English</a> · <strong>简体中文</strong></p>
</div>

OrcaRein 是一个小巧、可魔改的终端 agent：你和模型对话，它能读写文件、跑 shell 命令、操作你的项目——每个工具的执行都经过一道权限确认。它逐 token 流式输出、记住多轮上下文、把会话持久化到磁盘供你随时续聊。

> **状态 — v0.1.0 MVP。** 可用，但还在 1.0 之前：接口可能变动，安全模型刻意做得很薄（见 [安全](#安全)）。请在有版本控制或有备份的代码上使用。

## 特性

- **流式对话 REPL** —— 逐 token 输出，模型的"思考"阶段与"回复"分开显示。
- **5 个内置工具**，模型可调用：`read_file`、`write_file`、`list_dir`、`bash`（跨平台）、`edit`（唯一匹配替换）。
- **逐工具权限确认** —— 高风险工具执行前询问（`y`/`n`/`A`lways/`N`ever），允许/拒绝的决定在本次会话内缓存。默认拒绝（deny-by-default）。
- **可插拔 provider** —— DeepSeek 与 OpenAI 内置，统一在一个 `Provider` trait 之后；用 `--provider` 或 `ORCAREIN_PROVIDER` 切换。
- **分层配置** —— `config.toml` + 环境变量 + CLI 参数，按 CLI > 环境 > 文件 > 内置默认 解析。API key 只存在环境变量或 `0600` 权限的 `secrets.toml` 里，**绝不**走命令行。
- **会话持久化** —— 每轮自动存盘；`session list` / `session resume <id>` 接着上次继续。
- **`doctor`** —— 一条离线健康检查命令，开聊前告诉你 OrcaRein 是否配置就绪。
- **脚本 / 非交互模式** —— `--no-permission`（仅非 tty）、`--tools` 白名单、`--system-prompt-file`，供 headless 使用。

## 安装

需要 Rust **1.85+**（MSRV）。从源码构建：

```sh
git clone https://github.com/NickChuCode/orcarein
cd orcarein
cargo install --path crates/orcarein     # 安装 `orcarein` 可执行文件
# —— 或直接在 workspace 里运行：
cargo run --quiet
```

## 快速开始

```sh
# 1. 设置你的 provider API key（只走环境变量——绝不写进仓库里的参数/文件）。
export DEEPSEEK_API_KEY="sk-..."          # PowerShell: $env:DEEPSEEK_API_KEY = "sk-..."

# 2. 检查配置。
orcarein doctor

# 3. 开聊。
orcarein
```

示例会话：

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

REPL 内的 slash 命令：`/help`、`/clear`、`/save`、`/usage`、`/exit`。

## 用法

```
orcarein [MODEL] [OPTIONS]          # 启动对话 REPL
orcarein doctor                     # 离线健康检查
orcarein config get|set|list        # 管理 config.toml
orcarein session list|resume <id>   # 管理已存会话
```

| 参数 | 含义 |
|---|---|
| `[MODEL]` | provider 专属模型 id（覆盖 config + provider 默认值） |
| `--provider <name>` | `deepseek`（默认）或 `openai` |
| `--tools <csv>` | 工具白名单，如 `read_file,list_dir` |
| `--no-permission` | 跳过权限确认 —— **仅限非 tty stdin**（安全护栏） |
| `--system-prompt-file <path>` | 从文件读取 system prompt |

环境变量：`DEEPSEEK_API_KEY` / `OPENAI_API_KEY`、`ORCAREIN_PROVIDER`、`RUST_LOG`（如 `RUST_LOG=orcarein=debug` 看诊断日志）。

## 配置

解析优先级：**CLI 参数 > 环境变量 > `config.toml` > 内置默认。**

- `config.toml` —— 非密偏好（`provider`、`model`、`tools`、`system_prompt`）。用 `orcarein config set provider openai` 管理。
- `secrets.toml` —— API key，Unix 下以 `0600` 写入。key 也会从环境变量读取（环境优先）。**绝不**把 key 作为 CLI 参数传入。

路径按平台而定（Linux 走 XDG，Windows 走 `%APPDATA%`）；运行 `orcarein doctor` 可看到确切位置。

## 工具

| 工具 | 风险 | 作用 |
|---|---|---|
| `read_file` | safe | 读取一个 UTF-8 文本文件 |
| `list_dir` | risky | 列目录（文件名可能泄露结构） |
| `write_file` | risky | 写入/覆盖文件（父目录须已存在） |
| `edit` | risky | 替换文件中**唯一匹配**的子串 |
| `bash` | risky | 运行 shell 命令（`bash -c` / `cmd /C`） |

除非你对某工具缓存了"always"决定，或传了 `--no-permission`，否则 risky 工具都会触发权限确认。

## 安全

**OrcaRein 不是沙箱。** 权限确认是 v0.1 里**唯一**的安全层，而且刻意做得很薄：

- `bash`、`write_file`、`edit` 以**你的用户完整权限**运行。一旦你允许调用，模型就可能删除/覆盖文件，或执行任意命令。
- 确认是**默认拒绝**（空输入 / EOF / 任何意外输入 → 拒绝），但你一旦对某工具回答"always"，本次会话内它就不再询问、直接执行。
- `--no-permission` 会完全关闭确认。它在交互式终端下会被拒绝，只在管道（非 tty）stdin 下生效——只在你信任的脚本里使用。

请在有版本控制或有备份的代码上运行 OrcaRein。真正的沙箱是 1.0 之后的目标。

## 开发

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

对外契约由测试承载（`crates/*/tests/` 与 `#[cfg(test)]` 模块）。见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

双许可，任选其一：[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)。

## 致谢

架构与工具设计参考 [claw-code](https://github.com/ultraworkers/claw-code)。
