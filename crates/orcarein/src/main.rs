//! OrcaRein CLI — interactive chat REPL with streaming, session state,
//! tool dispatch, permission gating, and pluggable model providers.
//!
//! Chapter 14 milestone: a layered config system. `clap` (derive) parses the
//! CLI and a `config get/set/list` subcommand; effective settings resolve in
//! the order **CLI flag > env var > `config.toml` > built-in default**. API
//! keys come from the env or `secrets.toml` (never a CLI flag), via
//! `SecretStore`.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use orcarein_core::{
    env_key_var, BashTool, ChatOptions, Config, Decision, DeepSeekProvider, EditTool, ListDirTool,
    Message, OpenAIProvider, PermissionStore, Provider, ReadFileTool, RiskLevel, SecretStore,
    Session, SessionStore, SessionSummary, StreamEvent, TokenUsage, Tool, ToolCall, ToolDefinition,
    ToolRegistry, WriteFileTool,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

/// Fallback system prompt when neither `--system-prompt-file` nor the config
/// file supplies one.
const DEFAULT_SYSTEM_PROMPT: &str = "You are OrcaRein, a concise and helpful CLI assistant.";

/// Upper bound on tool-call iterations within one user turn. Prevents an
/// over-eager model from spinning the dispatcher.
const MAX_TOOL_ITERATIONS: usize = 8;

/// Names of every built-in tool — keeps `--tools` typo warnings honest.
const KNOWN_TOOLS: &[&str] = &["read_file", "write_file", "list_dir", "bash", "edit"];

/// Outcome of handling a slash command — whether the loop continues or quits.
enum CommandAction {
    Continue,
    Quit,
}

/// OrcaRein — an open-source CLI agent harness for DeepSeek V4 and
/// OpenAI-compatible models.
#[derive(Parser, Debug)]
#[command(name = "orcarein", version, about, long_about = None)]
struct Cli {
    /// Provider-specific model id (overrides config and the provider default).
    #[arg(value_name = "MODEL")]
    model: Option<String>,

    /// Backend provider: `deepseek` (default) or `openai`.
    #[arg(long)]
    provider: Option<String>,

    /// Skip permission prompts. Requires non-tty stdin (a safety guard).
    #[arg(long)]
    no_permission: bool,

    /// Comma-separated tool whitelist (e.g. `read_file,list_dir`).
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Read the system prompt from a file instead of the built-in default.
    #[arg(long, value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Get, set, or list persisted configuration in `config.toml`.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List saved sessions or resume one by id.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

/// Actions for the `config` subcommand. Mirrors the `Config::get/set/entries`
/// surface in `orcarein-core`.
#[derive(Subcommand, Debug, Clone)]
enum ConfigAction {
    /// Print the value of one key (`provider`, `model`, `tools`, `system_prompt`).
    Get { key: String },
    /// Set a key and persist it to `config.toml`.
    Set { key: String, value: String },
    /// List every key and its current value.
    List,
}

/// Actions for the `session` subcommand.
#[derive(Subcommand, Debug, Clone)]
enum SessionAction {
    /// List saved sessions, newest first.
    List,
    /// Resume a saved session by id, then continue chatting.
    Resume { id: String },
}

/// Effective settings after resolving CLI > env > config > defaults.
struct Resolved {
    provider: Box<dyn Provider>,
    model: String,
    system_prompt: String,
    tools_allowlist: Option<Vec<String>>,
    config_path: Option<PathBuf>,
}

/// Builds a `Provider` from a resolved name + optional API key. Validates the
/// provider name first so a missing key never masks a typo'd provider.
fn build_provider(name: &str, api_key: Option<String>) -> Result<Box<dyn Provider>> {
    match name {
        "deepseek" | "openai" => {}
        other => bail!("unknown provider: '{other}' (expected: deepseek | openai)"),
    }
    let var = env_key_var(name).expect("provider validated above");
    let key = api_key.with_context(|| {
        format!(
            "no API key for provider '{name}'. In PowerShell: $env:{var} = '<your-key>' \
             (or store it once in secrets.toml)"
        )
    })?;
    match name {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(key))),
        "openai" => Ok(Box::new(OpenAIProvider::new(key))),
        _ => unreachable!("provider validated above"),
    }
}

/// Treats a blank string as absent, so `--provider ""` or `ORCAREIN_PROVIDER=`
/// (set-but-empty) fall through to the next precedence layer rather than
/// becoming a literal empty value.
fn non_blank(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

/// Resolves effective settings from the precedence chain
/// CLI flag > env var > `config.toml` > built-in default.
fn resolve(cli: &Cli) -> Result<Resolved> {
    let config = Config::load().context("failed to load config.toml")?;

    let provider_name = non_blank(cli.provider.clone())
        .or_else(|| non_blank(std::env::var("ORCAREIN_PROVIDER").ok()))
        .or_else(|| non_blank(config.provider.clone()))
        .unwrap_or_else(|| "deepseek".to_owned());

    let secrets = SecretStore::load().context("failed to load secrets.toml")?;
    let provider = build_provider(&provider_name, secrets.resolve(&provider_name))?;

    let model = non_blank(cli.model.clone())
        .or_else(|| non_blank(config.model.clone()))
        .unwrap_or_else(|| provider.default_model().to_owned());

    let tools_allowlist = cli.tools.clone().or_else(|| config.tools_allowlist.clone());

    let system_prompt = match &cli.system_prompt_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read --system-prompt-file {}", path.display()))?,
        None => config
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_owned()),
    };

    Ok(Resolved {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        config_path: Config::config_path(),
    })
}

/// Runs an `orcarein config get/set/list` invocation, then returns.
fn run_config(action: ConfigAction) -> Result<()> {
    let mut config = Config::load().context("failed to load config.toml")?;
    match action {
        ConfigAction::Get { key } => match config.get(&key)? {
            Some(v) => println!("{v}"),
            None => println!("(unset)"),
        },
        ConfigAction::Set { key, value } => {
            config.set(&key, &value)?;
            config.save().context("failed to save config.toml")?;
            let where_ = Config::config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(unknown path)".to_owned());
            println!("set {key} = {value}  →  {where_}");
        }
        ConfigAction::List => {
            for (k, v) in config.entries() {
                println!("{k} = {}", v.as_deref().unwrap_or("(unset)"));
            }
        }
    }
    Ok(())
}

/// Runs `orcarein session list`, then returns.
fn run_session_list() -> Result<()> {
    let store = SessionStore::new().context("failed to locate session storage")?;
    let sessions = store.list().context("failed to list sessions")?;
    if sessions.is_empty() {
        println!("No saved sessions yet. ({})", store.dir().display());
        return Ok(());
    }
    println!("{:<15}  {:<9}  {:>5}  TITLE", "ID", "AGE", "TURNS");
    let now = SessionStore::now_ms();
    for SessionSummary {
        id,
        created_at_ms,
        turns,
        title,
    } in sessions
    {
        println!(
            "{:<15}  {:<9}  {:>5}  {}",
            id,
            format_age(now, created_at_ms),
            turns,
            title
        );
    }
    Ok(())
}

/// Formats the gap between two Unix-ms timestamps as a coarse "N ago" string.
/// Pure integer math — no calendar crate needed.
fn format_age(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Builds the tool registry honoring `--tools`. Warns to stderr if the
/// allowlist names any unknown tool.
fn build_registry(allowlist: Option<&[String]>) -> ToolRegistry {
    let all_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ReadFileTool),
        Box::new(WriteFileTool),
        Box::new(ListDirTool),
        Box::new(BashTool),
        Box::new(EditTool),
    ];

    if let Some(list) = allowlist {
        let known: std::collections::HashSet<&str> = KNOWN_TOOLS.iter().copied().collect();
        for name in list {
            if !known.contains(name.as_str()) {
                eprintln!("warning: --tools listed unknown tool '{name}' (ignored)");
            }
        }
    }

    let allow_set: Option<std::collections::HashSet<String>> =
        allowlist.map(|list| list.iter().cloned().collect());

    let mut registry = ToolRegistry::new();
    for tool in all_tools {
        let keep = match &allow_set {
            Some(set) => set.contains(tool.name()),
            None => true,
        };
        if keep {
            registry.register(tool);
        }
    }
    registry
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Subcommands that fully short-circuit the REPL. `session resume` does NOT
    // short-circuit — it yields an id we carry into the REPL below.
    let mut resume_id: Option<String> = None;
    match cli.command.clone() {
        Some(Command::Config { action }) => return run_config(action),
        Some(Command::Session {
            action: SessionAction::List,
        }) => return run_session_list(),
        Some(Command::Session {
            action: SessionAction::Resume { id },
        }) => resume_id = Some(id),
        None => {}
    }

    if cli.no_permission && std::io::stdin().is_terminal() {
        bail!(
            "--no-permission requires non-tty stdin (e.g. piped input) — refusing in interactive mode"
        );
    }

    // Resolve the session store and, if resuming, load the saved session *now* —
    // before `resolve()` demands an API key — so `resume <bad-id>` fails fast
    // with a "no such session" error rather than a confusing key error.
    let store = SessionStore::new().context("failed to locate session storage")?;
    let resumed = match &resume_id {
        Some(id) => {
            let loaded = store
                .load(id)
                .with_context(|| format!("failed to resume session '{id}'"))?;
            let created = store
                .created_at(id)
                .unwrap_or_else(|_| SessionStore::now_ms());
            Some((loaded, id.clone(), created))
        }
        None => None,
    };

    let resolved = resolve(&cli)?;
    let Resolved {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        config_path,
    } = resolved;

    // Either continue the resumed session (keeping its id + creation time so
    // auto-save writes back to the same file) or start a fresh one.
    let (mut session, session_id, created_at_ms) = match resumed {
        Some((loaded, id, created)) => {
            println!("Resumed session {id} ({} turns).", loaded.turn_count());
            (loaded, id, created)
        }
        None => {
            let created = SessionStore::now_ms();
            (Session::new(&system_prompt), created.to_string(), created)
        }
    };
    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;
    let mut permissions = PermissionStore::new();

    let registry = build_registry(tools_allowlist.as_deref());
    let tool_defs = registry.definitions();

    println!("OrcaRein — chat with {model}. /help for commands, Ctrl+D to quit.");
    println!("Provider: {}", provider.name());
    println!(
        "Config: {}",
        config_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_owned())
    );
    println!("Tools: {}", registry.names().join(", "));
    println!(
        "Session: {session_id} (auto-saved to {})",
        store.path_for(&session_id).display()
    );
    if cli.no_permission {
        println!("Permissions: DISABLED (--no-permission)\n");
    } else {
        println!(
            "Permissions: prompt on Risky tools (use --no-permission with piped input to skip)\n"
        );
    }

    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e).context("line editor failed"),
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        if let Some(stripped) = input.strip_prefix('/') {
            match handle_command(stripped, &mut session, &store, &session_id, created_at_ms) {
                CommandAction::Continue => continue,
                CommandAction::Quit => break,
            }
        }

        let _ = editor.add_history_entry(input);
        session.push_user(input);

        if let Err(e) = run_turn(
            provider.as_ref(),
            &model,
            &mut session,
            &registry,
            &tool_defs,
            &mut permissions,
            cli.no_permission,
        )
        .await
        {
            eprintln!("\n[错误] {e:#}\n");
            session.pop_last();
        } else if let Err(e) = store.save(&session_id, created_at_ms, &session) {
            // Auto-save after a successful turn; never let a save error
            // interrupt the conversation.
            eprintln!("[warn] 自动保存失败：{e}");
        }
    }

    println!("再见。");
    Ok(())
}

/// Runs one user turn: drives `provider.chat_stream`, executes any
/// returned tool calls, feeds results back, and repeats until the model
/// stops calling tools or `MAX_TOOL_ITERATIONS` is reached.
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    provider: &dyn Provider,
    model: &str,
    session: &mut Session,
    registry: &ToolRegistry,
    tool_defs: &[ToolDefinition],
    permissions: &mut PermissionStore,
    no_permission: bool,
) -> Result<()> {
    let opts = ChatOptions::new(model);
    let mut turn_usage = TokenUsage::default();
    let mut iteration = 0usize;

    loop {
        if iteration >= MAX_TOOL_ITERATIONS {
            eprintln!("[超过 tool call 上限 {MAX_TOOL_ITERATIONS} 次，中断]");
            break;
        }

        let mut stream = provider
            .chat_stream(session.messages(), tool_defs, &opts)
            .await?;

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut started_reasoning = false;
        let mut started_content = false;

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::Reasoning(text) => {
                    if !started_reasoning {
                        println!("[思考]");
                        started_reasoning = true;
                    }
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                    reasoning.push_str(&text);
                }
                StreamEvent::Content(text) => {
                    if !started_content {
                        if started_reasoning {
                            println!("\n");
                        }
                        println!("[回复]");
                        started_content = true;
                    }
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                    content.push_str(&text);
                }
                StreamEvent::ToolCalls(calls) => tool_calls = calls,
                StreamEvent::Usage(u) => turn_usage.add(u),
            }
        }
        println!();

        let assistant_msg = if tool_calls.is_empty() {
            Message::assistant(&content).with_reasoning(reasoning)
        } else {
            Message::assistant_with_tool_calls(&content, tool_calls.clone())
                .with_reasoning(reasoning)
        };
        session.push_assistant(assistant_msg);

        if tool_calls.is_empty() {
            break;
        }

        for call in &tool_calls {
            let result = dispatch(registry, permissions, no_permission, call).await;
            session.push_assistant(Message::tool(&call.id, result));
        }

        iteration += 1;
    }

    session.record_usage(turn_usage);
    eprintln!(
        "[tokens: +{} this turn / {} total]\n",
        turn_usage.total_tokens,
        session.usage().total_tokens
    );
    Ok(())
}

/// Executes a single tool call against the registry, gating `Risky`
/// tools behind a permission prompt unless the user has cached an
/// `AllowAlways` decision or invoked OrcaRein with `--no-permission`.
async fn dispatch(
    registry: &ToolRegistry,
    permissions: &mut PermissionStore,
    no_permission: bool,
    call: &ToolCall,
) -> String {
    eprintln!(
        "[tool: {}({})]",
        call.function.name, call.function.arguments
    );

    let args = match call.function.parse_arguments() {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("ERROR: bad arguments JSON: {e}");
            eprintln!("[tool error] {msg}");
            return msg;
        }
    };

    let Some(tool) = registry.get(&call.function.name) else {
        let msg = format!("ERROR: unknown tool '{}'", call.function.name);
        eprintln!("[tool error] {msg}");
        return msg;
    };

    if tool.risk_level() == RiskLevel::Risky && !no_permission {
        let decision = match permissions.cached(tool.name()) {
            Some(d) => d,
            None => {
                let d = prompt_permission(tool.name(), &call.function.arguments);
                if d.is_sticky() {
                    permissions.remember(tool.name(), d);
                }
                d
            }
        };
        if !decision.is_allow() {
            let msg = format!("ERROR: user denied permission for '{}'", tool.name());
            eprintln!("[denied] {msg}");
            return msg;
        }
    }

    match tool.execute(args).await {
        Ok(out) => {
            eprintln!("[result] {} bytes", out.content.len());
            out.content
        }
        Err(e) => {
            let msg = format!("ERROR: {e}");
            eprintln!("[tool error] {msg}");
            msg
        }
    }
}

/// Synchronously prompts the user. Any input we cannot parse — empty
/// line, EOF, IO error — collapses to `DenyOnce` (deny-by-default).
fn prompt_permission(name: &str, args: &str) -> Decision {
    eprintln!();
    eprintln!("OrcaRein wants to run: {name}({args})");
    eprint!("Allow? [y=once N=never A=always n=once]: ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Decision::DenyOnce;
    }
    match line.trim().chars().next() {
        Some('y') => Decision::AllowOnce,
        Some('A') => Decision::AllowAlways,
        Some('N') => Decision::DenyAlways,
        _ => Decision::DenyOnce,
    }
}

/// Handles a slash command (the leading `/` already stripped). Takes the
/// session store + active id so `/save` can persist on demand.
fn handle_command(
    cmd: &str,
    session: &mut Session,
    store: &SessionStore,
    session_id: &str,
    created_at_ms: u64,
) -> CommandAction {
    match cmd {
        "exit" | "quit" => CommandAction::Quit,
        "clear" => {
            session.clear();
            println!("(会话已清空，system prompt 保留)");
            CommandAction::Continue
        }
        "save" => {
            match store.save(session_id, created_at_ms, session) {
                Ok(()) => println!("已保存：{}", store.path_for(session_id).display()),
                Err(e) => eprintln!("保存失败：{e}"),
            }
            CommandAction::Continue
        }
        "usage" => {
            let u = session.usage();
            println!(
                "[累计 tokens: prompt {} / completion {} / total {}; 当前 {} 轮]",
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                session.turn_count()
            );
            CommandAction::Continue
        }
        "help" => {
            println!("命令：");
            println!("  /exit, /quit   退出");
            println!("  /clear         清空会话（保留 system prompt）");
            println!("  /save          立即保存会话到磁盘（每轮也会自动保存）");
            println!("  /usage         显示累计 token 用量");
            println!("  /help          这条帮助");
            CommandAction::Continue
        }
        other => {
            eprintln!("未知命令：/{other}。/help 查看支持的命令。");
            CommandAction::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        // Catches derive-level mistakes (e.g. a positional clashing with a
        // subcommand) at test time instead of first run.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_args_is_repl() {
        let cli = Cli::try_parse_from(["orcarein"]).unwrap();
        assert!(cli.model.is_none());
        assert!(cli.command.is_none());
        assert!(!cli.no_permission);
    }

    #[test]
    fn positional_model_parses() {
        let cli = Cli::try_parse_from(["orcarein", "deepseek-v4-pro"]).unwrap();
        assert_eq!(cli.model.as_deref(), Some("deepseek-v4-pro"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn provider_and_csv_tools_parse() {
        let cli = Cli::try_parse_from([
            "orcarein",
            "--provider",
            "openai",
            "--tools",
            "read_file,list_dir",
        ])
        .unwrap();
        assert_eq!(cli.provider.as_deref(), Some("openai"));
        assert_eq!(
            cli.tools.as_deref(),
            Some(&["read_file".to_owned(), "list_dir".to_owned()][..])
        );
    }

    #[test]
    fn config_set_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "config", "set", "provider", "openai"]).unwrap();
        match cli.command {
            Some(Command::Config {
                action: ConfigAction::Set { key, value },
            }) => {
                assert_eq!(key, "provider");
                assert_eq!(value, "openai");
            }
            other => panic!("expected config set, got {other:?}"),
        }
        // A subcommand must not be mistaken for the positional model.
        assert!(cli.model.is_none());
    }

    #[test]
    fn config_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "config", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::List
            })
        ));
    }

    #[test]
    fn session_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "session", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                action: SessionAction::List
            })
        ));
    }

    #[test]
    fn session_resume_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "session", "resume", "1748789422123"]).unwrap();
        match cli.command {
            Some(Command::Session {
                action: SessionAction::Resume { id },
            }) => assert_eq!(id, "1748789422123"),
            other => panic!("expected session resume, got {other:?}"),
        }
        assert!(cli.model.is_none());
    }

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(30_000, 0), "30s ago");
        assert_eq!(format_age(120_000, 0), "2m ago");
        assert_eq!(format_age(7_200_000, 0), "2h ago");
        assert_eq!(format_age(2 * 86_400_000, 0), "2d ago");
        // A clock skew where "then" is in the future must not panic.
        assert_eq!(format_age(0, 5_000), "0s ago");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["orcarein", "--nope"]).is_err());
    }

    #[test]
    fn non_blank_treats_empty_as_absent() {
        // A set-but-empty env var or `--provider ""` must fall through, not
        // become a literal empty provider name.
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some("   ".to_owned())), None);
        assert_eq!(
            non_blank(Some("openai".to_owned())),
            Some("openai".to_owned())
        );
    }
}
