<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo_dark.png">
    <img src="assets/logo_light.png" alt="OrcaRein" width="280">
  </picture>

  <p><em>一个开源的 Rust CLI agent harness，面向 DeepSeek V4 及任意 OpenAI 兼容大模型。</em></p>

  <p>
    <a href="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml"><img src="https://github.com/NickChuCode/orcarein/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="#许可证"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
    <a href="https://github.com/NickChuCode/orcarein/releases/latest"><img src="https://img.shields.io/github/v/release/NickChuCode/orcarein?sort=semver" alt="Latest release"></a>
  </p>

  <p><a href="README.md">English</a> · <strong>简体中文</strong></p>
</div>

**OrcaRein** 是一个小巧、可魔改的终端 agent：你和模型对话，它就能操作你的项目——读取与检索文件、修改代码、执行 shell 命令、调用外部工具——每一个高风险动作都经过一道逐工具的权限确认。它逐 token 流式输出、保留多轮上下文、把会话持久化到磁盘供你随时续聊。

它以单个静态二进制发布（默认 DeepSeek，也支持任意 OpenAI 兼容端点），底层是一个可嵌入的 `orcarein-core` 库。它能带着你自己的 API key 在受限网络下工作，并且**诚实降级**：能力足够的终端上是丰富的 TUI，管道 / CI / headless SBC 上则是干净的纯文本输出。

> **状态 — v0.3.0。** 日常可用，但仍在 1.0 之前：接口可能变动，而且**尚无沙箱**——权限确认是目前唯一的安全层（见 [安全](#安全)）。请在有版本控制或有备份的代码上运行。真正的沙箱在 [路线图](#路线图) 上。

---

## 能力

### Agent 核心

- **流式对话 REPL** —— 逐 token 输出，模型的"思考"阶段与"回复"分开显示。
- **多轮工具调用循环**，带自动**工具调用修复**（畸形调用会被恢复而非中断这一轮）与自纠错协议。
- **成本与 token 计量** —— 每会话实时用量统计（`/usage`）。

### 内置工具

八个模型可调用的工具，各自声明风险级别：

| 工具 | 风险 | 作用 |
|---|---|---|
| `read_file` | safe | 读取一个 UTF-8 文本文件 |
| `search` | risky | 全树正则检索，**感知 .gitignore**，返回 `path:line:text` |
| `list_dir` | risky | 列目录 |
| `write_file` | risky | 写入 / 覆盖文件 |
| `edit` | risky | 替换文件中**唯一匹配**的子串 |
| `bash` | risky | 执行 shell 命令（`bash -c` / `cmd /C`），输出封顶 32 KiB |
| `skill` | safe | 按需加载一个具名指令包（见 [技能](#技能与子-agent)） |
| `task` | safe | 把子任务委派给隔离上下文的子 agent（见 [子 agent](#技能与子-agent)） |

外加从配置动态注册的任意 **MCP-server 工具**。

### 权限与安全

- **逐工具权限确认** —— 高风险工具执行前询问（`y` / `n` / `A`lways / `N`ever）。**默认拒绝**：空输入、EOF 或任何意外输入都拒绝该调用。
- 允许 / 拒绝的决定在**本次会话内缓存**；对某工具回答一次"always"，本次运行内它就不再询问。

### Provider 与模型

- **可插拔 provider** —— DeepSeek（默认）与 OpenAI 内置，统一在一个 `Provider` trait 之后；用 `--provider` 或 `ORCAREIN_PROVIDER` 切换。任意 OpenAI 兼容端点均可。
- **rustls TLS** —— 不用 OpenSSL，交叉编译到 aarch64 Linux（树莓派 / 香橙派）无痛，二进制自带根证书。
- **瞬时失败重试** —— 429 / 5xx / 网络抖动在流开始前按指数退避重试；可通过 `[retry]` 配置段调整。
- **运行时切换模型** —— `/model` 实时切换并持久化选择；编辑器内的选择器列出从 provider `GET /v1/models` 拉取的模型，新模型自动出现、退役模型自动消失。

### 会话与上下文

- **会话持久化** —— 每轮自动存盘；`session list` / `session resume <id>` / `session delete <id>`，以及运行时 `/sessions`、`/resume`、`/new`。
- **手动上下文压缩** —— `/compact` 拍平并摘要对话以回收上下文窗口（并诚实提示 cache 代价）。
- **项目记忆** —— 仓库的 `AGENTS.md`（从 cwd 向上查找）会并入 system prompt；`/init` 生成一个。

### 技能与子 agent

- **技能（Skills）** —— 具名、按需的指令包，从 `.orcarein/skills/*.md`（或 `<name>/SKILL.md`）发现。prompt 里只放一个轻量索引；模型在相关时通过 `skill` 工具拉取技能正文。用 `/skills` 浏览。
- **子 agent** —— `task` 工具把自包含的子任务委派给一个全新的子 agent，它有**自己隔离的上下文窗口**；只把简洁结果带回，保持主对话干净。
- **MCP 客户端** —— 手写的 Model Context Protocol stdio 客户端（默认开的 `mcp` feature），启动时从配置的 `[[mcp_servers]]` 段注册外部工具。

### 终端体验

- **Vim 模态编辑器** —— 自研多行输入（normal / insert / visual 模式，motion、operator、count、undo/redo、OSC52 剪贴板），终端不支持时退化为普通行编辑器。
- **`@`-mention 补全** —— 编辑器内弹窗补全项目文件与目录（感知 .gitignore）；提交时 `@path` 注入文件内容或目录树，让模型看到而不污染缓存前缀。
- **带语法高亮的 Markdown 分页器** —— `/show` 与 `/history` 渲染标题、粗 / 斜体、代码块、引用、列表、链接、以及 CJK 对齐的表格，配一个手写零依赖的代码高亮器。
- **语义色彩系统** —— 8 个具名 token，truecolor → 256 → 16 → `NO_COLOR` 逐档降级，统一头部展示 模型 / cwd / 会话。
- **诚实降级** —— 非 tty、`TERM=dumb`、串口、headless 环境都退回干净的纯文本打印。

### Headless 与脚本

- **`run`** —— 非交互执行单个任务（prompt 作为参数或从 stdin 读）；答案走 stdout，诊断走 stderr。
- **`--no-permission`**（仅非 tty stdin）、**`--tools`** 白名单、**`--system-prompt-file`**，供无人值守使用。
- **`issue <N>`** —— BYO-key 自举闭环：读取一个 GitHub issue，让 agent 改代码（read/list/edit/write，无 shell），跑 `cargo test`，把 diff 给你看。它绝不 commit、push 或开 PR——那是你的决定。
- **`doctor`** —— 离线健康检查，开聊前告诉你 OrcaRein 是否配置就绪。

### 工程质量

360+ 测试、三平台 CI 矩阵（Linux / macOS / Windows）、MSRV **1.85**、Conventional Commits、`release-plz` 维护的 changelog，以及每次发布产出 **6 个目标平台二进制**的 tag 驱动管线。

---

## 路线图

> 🚧 **开发中 —— 计划中，尚未发布。** 这些是靶子不是承诺，日期与范围可能调整。进度见 [releases](https://github.com/NickChuCode/orcarein/releases) 页。

| 能力 | 增加什么 | 目标版本 |
|---|---|---|
| **权限 v2** | `allow` / `ask` / `deny` 规则（命令 + 路径模式）、持久化配置、权限档位（`default` / `acceptEdits` / `plan` / `yolo`）、路径风险升级（`~/.ssh`、`.env`、`*.pem`） | v0.4.0 |
| **Hooks** | `PreToolUse` / `PostToolUse` 生命周期 hook —— 由退出码强制执行的护栏，而非 prompt 请求 | v0.4.0 |
| **自动压缩 + 工具输出修剪** | 阈值触发压缩 + `prompt_too_long` 反应式兜底；非破坏性修剪陈旧工具输出；保护前缀让缓存存活 | v0.5.0 |
| **沙箱** | 新 `orcarein-sandbox` crate —— Linux Landlock + seccomp（默认 workspace-write + 断网）、macOS Seatbelt、Windows 诚实降级 | v0.6.0 |
| **MCP over HTTP** | Streamable HTTP 传输、`.mcp.json` 项目配置、deferred 工具 schema（空闲 MCP 工具不膨胀前缀）；以 Playwright-MCP 看网页为验收标杆 | v0.7.0 |
| **自定义斜杠命令** | `.orcarein/commands/*.md`，复用技能的发现 / 解析路径 | v0.7.0 |
| **并行子 agent** | `task` 并行 fan-out + 自定义 agent 类型（Markdown frontmatter 定义 模型 / 工具白名单 / 人设） | v0.7.0 |
| **Repomap** | tree-sitter 符号索引，全仓代码智能，离线可用 | v0.7.0 |
| **检查点与回滚** | side-git 快照 + `/restore` 撤销一次运行的改动 | v0.8.0 |
| **流式 Markdown** | token 流式过程中渐进渲染 Markdown | v0.8.0 |
| **Web 工具** | `web_fetch` + `web_search`，`SearchBackend` trait（国内 Bocha / 国际 Tavily）—— 受限网络下可用 | v0.8.0 |
| **Plan 模式 / todo 工具** | 轻量规划 + 任务追踪面 | v0.8.0 |
| **更广分发** | 恢复 crates.io、npm / brew 封装、生态名单收录 | v0.9.0 |

**1.0** 以门槛（而非日期）触发：`orcarein-core` 公共 API 冻结并审计通过。

---

## 安装

### 预编译二进制（推荐）

从[最新 release](https://github.com/NickChuCode/orcarein/releases/latest) 下载对应平台的压缩包
（Linux x86_64 / aarch64、macOS x86_64 / arm64、Windows x86_64），解压后把 `orcarein` 可执行文件放进 `PATH`。

### 从源码

需要 Rust **1.85+**（MSRV）：

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
Tools: bash, edit, list_dir, read_file, search, write_file
> 这个仓库有几个 workspace 成员？
[思考] 我读一下 workspace 清单。
[tool: read_file({"path":"Cargo.toml"})]
[result] 641 bytes
[回复] 四个：orcarein-core、orcarein、orcarein-hardware、orcarein-eval。
```

## 用法

```
orcarein [MODEL] [OPTIONS]          # 启动对话 REPL
orcarein run [PROMPT]               # 非交互执行单个任务（headless）
orcarein issue <N>                  # 在当前仓库修复 GitHub issue #N
orcarein doctor                     # 离线健康检查
orcarein config get|set|list        # 管理 config.toml
orcarein session list|resume|delete  # 管理已存会话
```

| 参数 | 含义 |
|---|---|
| `[MODEL]` | provider 专属模型 id（覆盖 config + provider 默认值） |
| `--provider <name>` | `deepseek`（默认）或 `openai` |
| `--tools <csv>` | 工具白名单，如 `read_file,search` |
| `--no-permission` | 跳过权限确认 —— **仅限非 tty stdin**（安全护栏） |
| `--system-prompt-file <path>` | 从文件读取 system prompt |

**斜杠命令**（REPL 内）：`/help`、`/clear`、`/model`、`/tools`、`/skills`、`/compact`、`/usage`、`/save`、`/show`、`/history`、`/init`、`/sessions`、`/resume`、`/new`、`/exit`。

环境变量：`DEEPSEEK_API_KEY` / `OPENAI_API_KEY`、`ORCAREIN_PROVIDER`、`RUST_LOG`（如 `RUST_LOG=orcarein=debug` 看诊断日志）。

## 配置

解析优先级：**CLI 参数 > 环境变量 > `config.toml` > 内置默认。**

- `config.toml` —— 非密偏好（`provider`、`model`、`tools`、`system_prompt`、`[retry]`、`[[mcp_servers]]`）。用 `orcarein config set provider openai` 管理。
- `secrets.toml` —— API key，Unix 下以 `0600` 写入。key 也会从环境变量读取（环境优先）。**绝不**把 key 作为 CLI 参数传入。

路径按平台而定（Linux 走 XDG，Windows 走 `%APPDATA%`）；运行 `orcarein doctor` 可看到确切位置。

## 安全

**OrcaRein 不是沙箱**（目前还不是——见[路线图](#路线图)）。权限确认是当前**唯一**的安全层，而且刻意做得很薄：

- `bash`、`write_file`、`edit` 以**你的用户完整权限**运行。一旦你允许调用，模型就可能删除/覆盖文件，或执行任意命令。
- 确认是**默认拒绝**，但你一旦对某工具回答"always"，本次会话内它就不再询问、直接执行。
- `--no-permission` 会完全关闭确认。它在交互式终端下会被拒绝，只在管道（非 tty）stdin 下生效——只在你信任的脚本里使用。

请在有版本控制或有备份的代码上运行 OrcaRein。

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

早期架构与工具设计参考了 [claw-code](https://github.com/ultraworkers/claw-code) 的学习心得。
