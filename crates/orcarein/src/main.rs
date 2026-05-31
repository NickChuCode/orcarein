//! OrcaRein CLI — interactive chat REPL with streaming, session state,
//! tool dispatch, permission gating, and pluggable model providers.
//!
//! Chapter 13 milestone: the REPL talks to a `dyn Provider` rather than
//! a hard-wired DeepSeek module. `ORCAREIN_PROVIDER=deepseek|openai`
//! picks the backend at startup; `MockProvider` is available to
//! integration tests in `orcarein-core`. The provider returns a
//! `BoxStream<StreamEvent>` and the REPL renders + accumulates events
//! as they arrive.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use orcarein_core::{
    BashTool, ChatOptions, Decision, DeepSeekProvider, EditTool, ListDirTool, Message,
    OpenAIProvider, PermissionStore, Provider, ReadFileTool, RiskLevel, Session, StreamEvent,
    TokenUsage, Tool, ToolCall, ToolDefinition, ToolRegistry, WriteFileTool,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{IsTerminal, Write};

/// The system message that steers every conversation.
const SYSTEM_PROMPT: &str = "You are OrcaRein, a concise and helpful CLI assistant.";

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

/// Parsed command-line arguments.
struct Cli {
    /// Model id (may be `None` — meaning "use the provider's default").
    model: Option<String>,
    no_permission: bool,
    tools_allowlist: Option<Vec<String>>,
}

fn print_usage() {
    eprintln!("Usage: orcarein [MODEL] [--no-permission] [--tools <csv>]");
    eprintln!();
    eprintln!("Positional:");
    eprintln!("  MODEL                   Provider-specific model id (default: provider's choice)");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --no-permission         Skip permission prompts. Requires non-tty stdin.");
    eprintln!(
        "  --tools <csv>           Comma-separated tool whitelist (e.g. read_file,list_dir)."
    );
    eprintln!("  -h, --help              Print this help and exit.");
    eprintln!();
    eprintln!("Environment:");
    eprintln!("  ORCAREIN_PROVIDER        deepseek (default) | openai");
    eprintln!("  DEEPSEEK_API_KEY        Required when provider is `deepseek`.");
    eprintln!("  OPENAI_API_KEY          Required when provider is `openai`.");
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
        model,
        no_permission,
        tools_allowlist,
    })
}

/// Picks a `Provider` per `ORCAREIN_PROVIDER` (default `deepseek`).
/// Each branch reads its provider-specific API key env var.
fn select_provider() -> Result<Box<dyn Provider>> {
    let name = std::env::var("ORCAREIN_PROVIDER").unwrap_or_else(|_| "deepseek".into());
    match name.as_str() {
        "deepseek" => {
            let key = std::env::var("DEEPSEEK_API_KEY").context(
                "DEEPSEEK_API_KEY not set. In PowerShell: $env:DEEPSEEK_API_KEY = '<your-key>'",
            )?;
            Ok(Box::new(DeepSeekProvider::new(key)))
        }
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY").context(
                "OPENAI_API_KEY not set. In PowerShell: $env:OPENAI_API_KEY = '<your-key>'",
            )?;
            Ok(Box::new(OpenAIProvider::new(key)))
        }
        other => bail!("unknown ORCAREIN_PROVIDER: '{other}' (expected: deepseek | openai)"),
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
    let cli = parse_cli()?;

    if cli.no_permission && std::io::stdin().is_terminal() {
        bail!(
            "--no-permission requires non-tty stdin (e.g. piped input) — refusing in interactive mode"
        );
    }

    let provider = select_provider()?;
    let model = cli
        .model
        .clone()
        .unwrap_or_else(|| provider.default_model().to_owned());

    let mut session = Session::new(SYSTEM_PROMPT);
    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;
    let mut permissions = PermissionStore::new();

    let registry = build_registry(cli.tools_allowlist.as_deref());
    let tool_defs = registry.definitions();

    println!("OrcaRein — chat with {model}. /help for commands, Ctrl+D to quit.");
    println!("Provider: {}", provider.name());
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
