//! DeepRig CLI — interactive chat REPL with streaming, session state, and
//! tool dispatch.
//!
//! Chapter 10 milestone: the model can ask DeepRig to run tools. A
//! `ToolRegistry` holds the available `Box<dyn Tool>`s (just `read_file`
//! in Ch10), their `definitions()` ship in every request, and the REPL
//! runs a `tool_calls -> execute -> feed back -> re-query` loop until the
//! model is done — capped at `MAX_TOOL_ITERATIONS` so a misbehaving model
//! can't spin forever.

mod deepseek;

use anyhow::{Context, Result};
use deeprig_core::{
    BashTool, EditTool, ListDirTool, Message, ReadFileTool, Session, TokenUsage, ToolCall,
    ToolDefinition, ToolRegistry, WriteFileTool,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::Write;

use crate::deepseek::StreamEvent;

/// Model used when none is given on the command line.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// The system message that steers every conversation.
const SYSTEM_PROMPT: &str = "You are DeepRig, a concise and helpful CLI assistant.";

/// Upper bound on tool-call iterations within one user turn. Prevents an
/// over-eager model from spinning the dispatcher.
const MAX_TOOL_ITERATIONS: usize = 8;

/// Outcome of handling a slash command — whether the loop continues or quits.
enum CommandAction {
    Continue,
    Quit,
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .context("DEEPSEEK_API_KEY not set. In PowerShell: $env:DEEPSEEK_API_KEY = '<your-key>'")?;
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MODEL.into());

    let client = reqwest::Client::new();
    let mut session = Session::new(SYSTEM_PROMPT);
    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFileTool));
    registry.register(Box::new(WriteFileTool));
    registry.register(Box::new(ListDirTool));
    registry.register(Box::new(BashTool));
    registry.register(Box::new(EditTool));
    let tool_defs = registry.definitions();

    println!("DeepRig — chat with {model}. /help for commands, Ctrl+D to quit.");
    println!("Tools: {}\n", registry.names().join(", "));

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
            &model,
            &mut session,
            &registry,
            &tool_defs,
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
async fn run_turn(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    session: &mut Session,
    registry: &ToolRegistry,
    tool_defs: &[ToolDefinition],
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
            let result = dispatch(registry, call).await;
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

/// Executes a single tool call against the registry, formatting any
/// failure (bad args, unknown tool, IO error) as an `ERROR:` payload that
/// the model will see in the next round.
async fn dispatch(registry: &ToolRegistry, call: &ToolCall) -> String {
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
