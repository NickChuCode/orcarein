//! DeepRig CLI — interactive chat REPL with streaming + session state.
//!
//! Chapter 8 milestone: a real `Session` owns the conversation history and
//! cumulative token usage. Slash commands let the user reset the session,
//! see usage, and get help. The REPL prints per-turn token usage so the
//! cost of a long conversation is visible.

mod deepseek;

use anyhow::{Context, Result};
use deeprig_core::Session;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::Write;

use crate::deepseek::StreamEvent;

/// Model used when none is given on the command line.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// The system message that steers every conversation.
const SYSTEM_PROMPT: &str = "You are DeepRig, a concise and helpful CLI assistant.";

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

    println!("DeepRig — chat with {model}. /help for commands, Ctrl+D to quit.\n");

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

        match deepseek::chat_stream(&client, &api_key, &model, session.messages(), emit).await {
            Ok(outcome) => {
                println!();
                if let Some(u) = outcome.usage {
                    session.record_usage(u);
                    let total = session.usage();
                    eprintln!(
                        "[tokens: +{} this turn / {} total]\n",
                        u.total_tokens, total.total_tokens
                    );
                } else {
                    println!();
                }
                session.push_assistant(outcome.message);
            }
            Err(e) => {
                eprintln!("\n[错误] {e:#}\n");
                session.pop_last();
            }
        }
    }

    println!("再见。");
    Ok(())
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
