//! DeepRig CLI — interactive chat REPL with streaming, session state, tool
//! dispatch, and per-tool permission prompts.
//!
//! Chapter 12 milestone: every `Risky` tool (`bash`, `write_file`, `edit`)
//! is gated by an interactive `y / n / A / N` prompt unless the user
//! either has already answered `A`/`N` this session (sticky cache) or
//! passed `--no-permission` over piped stdin. `--tools <csv>` further
//! limits which tools get registered, which Ch24 will use for the
//! issue-bot scriptable runs.

mod deepseek;

use anyhow::{bail, Context, Result};
use deeprig_core::{
    BashTool, Decision, EditTool, ListDirTool, Message, PermissionStore, ReadFileTool, RiskLevel,
    Session, TokenUsage, Tool, ToolCall, ToolDefinition, ToolRegistry, WriteFileTool,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{IsTerminal, Write};

use crate::deepseek::StreamEvent;

/// Model used when none is given on the command line.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// The system message that steers every conversation.
const SYSTEM_PROMPT: &str = "You are DeepRig, a concise and helpful CLI assistant.";

/// Upper bound on tool-call iterations within one user turn. Prevents an
/// over-eager model from spinning the dispatcher.
const MAX_TOOL_ITERATIONS: usize = 8;

/// Names of every built-in tool, in registration order. Kept here so the
/// `--tools` allowlist can warn about typos against a single source of
/// truth.
const KNOWN_TOOLS: &[&str] = &["read_file", "write_file", "list_dir", "bash", "edit"];

/// Outcome of handling a slash command — whether the loop continues or quits.
enum CommandAction {
    Continue,
    Quit,
}

/// Parsed command-line arguments.
struct Cli {
    model: String,
    no_permission: bool,
    tools_allowlist: Option<Vec<String>>,
}

fn print_usage() {
    eprintln!("Usage: deeprig [MODEL] [--no-permission] [--tools <csv>]");
    eprintln!();
    eprintln!("Positional:");
    eprintln!("  MODEL                   DeepSeek model id (default: {DEFAULT_MODEL})");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --no-permission         Skip permission prompts. Requires non-tty stdin.");
    eprintln!(
        "  --tools <csv>           Comma-separated tool whitelist (e.g. read_file,list_dir)."
    );
    eprintln!("  -h, --help              Print this help and exit.");
}

fn parse_cli() -> Result<Cli> {
    let mut model: Option<String> = None;
    let mut no_permission = false;
    let mut tools_allowlist: Option<Vec<String>> = None;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--no-permission" => no_permission = true,
            "--tools" => {
                let val = iter
                    .next()
                    .context("--tools requires a comma-separated value")?;
                let list: Vec<String> = val
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                tools_allowlist = Some(list);
            }
            other if other.starts_with("--") => {
                print_usage();
                bail!("unknown flag: {other}");
            }
            other if model.is_none() => model = Some(other.to_owned()),
            other => {
                print_usage();
                bail!("unexpected extra positional argument: {other}");
            }
        }
    }

    Ok(Cli {
        model: model.unwrap_or_else(|| DEFAULT_MODEL.into()),
        no_permission,
        tools_allowlist,
    })
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
    let cli = parse_cli()?;

    // Safety guard: --no-permission must NEVER be silently honored in an
    // interactive shell. The flag exists for piped / scripted runs
    // (Ch24 issue bot), not for "just hide the prompts."
    if cli.no_permission && std::io::stdin().is_terminal() {
        bail!(
            "--no-permission requires non-tty stdin (e.g. piped input) — refusing in interactive mode"
        );
    }

    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .context("DEEPSEEK_API_KEY not set. In PowerShell: $env:DEEPSEEK_API_KEY = '<your-key>'")?;

    let client = reqwest::Client::new();
    let mut session = Session::new(SYSTEM_PROMPT);
    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;
    let mut permissions = PermissionStore::new();

    let registry = build_registry(cli.tools_allowlist.as_deref());
    let tool_defs = registry.definitions();

    println!(
        "DeepRig — chat with {model}. /help for commands, Ctrl+D to quit.",
        model = cli.model
    );
    println!("Tools: {}", registry.names().join(", "));
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
            match handle_command(stripped, &mut session) {
                CommandAction::Continue => continue,
                CommandAction::Quit => break,
            }
        }

        let _ = editor.add_history_entry(input);
        session.push_user(input);

        if let Err(e) = run_turn(
            &client,
            &api_key,
            &cli.model,
            &mut session,
            &registry,
            &tool_defs,
            &mut permissions,
            cli.no_permission,
        )
        .await
        {
            eprintln!("\n[错误] {e:#}\n");
            // Roll back the user turn so the next prompt is not stuck mid-loop.
            session.pop_last();
        }
    }

    println!("再见。");
    Ok(())
}

/// Runs one user turn: streams a reply, executes any tool calls, feeds the
/// results back, and repeats until the model returns a tool-call-free
/// message (or `MAX_TOOL_ITERATIONS` is reached).
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    session: &mut Session,
    registry: &ToolRegistry,
    tool_defs: &[ToolDefinition],
    permissions: &mut PermissionStore,
    no_permission: bool,
) -> Result<()> {
    let mut turn_usage = TokenUsage::default();
    let mut iteration = 0usize;

    loop {
        if iteration >= MAX_TOOL_ITERATIONS {
            eprintln!("[超过 tool call 上限 {MAX_TOOL_ITERATIONS} 次，中断]");
            break;
        }

        // Per-iteration emit state — printed banners reset each round so
        // a multi-turn tool loop still labels [思考] / [回复] cleanly.
        let mut started_reasoning = false;
        let mut started_content = false;
        let emit = |event: StreamEvent| {
            match event {
                StreamEvent::Reasoning(text) => {
                    if !started_reasoning {
                        println!("[思考]");
                        started_reasoning = true;
                    }
                    print!("{text}");
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
                }
            }
            let _ = std::io::stdout().flush();
        };

        let outcome =
            deepseek::chat_stream(client, api_key, model, session.messages(), tool_defs, emit)
                .await?;
        println!();

        if let Some(u) = outcome.usage {
            turn_usage.add(u);
        }

        // Clone tool_calls out so we can iterate after handing the
        // message over to the session — the model needs to see the
        // original assistant message (with tool_calls) on the next round
        // so it can bind our role=tool replies back to its requests.
        let tool_calls = outcome.message.tool_calls.clone();
        session.push_assistant(outcome.message);

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
/// `AllowAlways` decision or invoked DeepRig with `--no-permission`.
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
    eprintln!("DeepRig wants to run: {name}({args})");
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

/// Handles a slash command (the leading `/` already stripped).
fn handle_command(cmd: &str, session: &mut Session) -> CommandAction {
    match cmd {
        "exit" | "quit" => CommandAction::Quit,
        "clear" => {
            session.clear();
            println!("(会话已清空，system prompt 保留)");
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
