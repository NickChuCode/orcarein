//! DeepRig CLI — interactive chat REPL with streaming output.
//!
//! Chapter 7 milestone: deltas arrive incrementally; the REPL renders the
//! "thinking" phase and the final "reply" as they come in, instead of
//! freezing until the whole response is done.

mod deepseek;

use anyhow::{Context, Result};
use deeprig_core::Message;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::Write;

use crate::deepseek::StreamEvent;

/// Model used when none is given on the command line.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// The system message that steers every conversation.
const SYSTEM_PROMPT: &str = "You are DeepRig, a concise and helpful CLI assistant.";

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .context("DEEPSEEK_API_KEY not set. In PowerShell: $env:DEEPSEEK_API_KEY = '<your-key>'")?;
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MODEL.into());

    let client = reqwest::Client::new();
    let mut messages = vec![Message::system(SYSTEM_PROMPT)];

    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;

    println!("DeepRig — chat with {model}. Ctrl+D or /exit to quit.\n");

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
        if input == "/exit" || input == "/quit" {
            break;
        }
        let _ = editor.add_history_entry(input);

        messages.push(Message::user(input));

        // Phase tracking for typewriter output. The closure captures these
        // `&mut`; they live only for this turn.
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
            // Force the token onto the screen — print! is line-buffered.
            let _ = std::io::stdout().flush();
        };

        match deepseek::chat_stream(&client, &api_key, &model, &messages, emit).await {
            Ok(assistant) => {
                println!("\n"); // trailing newline after the stream ends
                messages.push(assistant);
            }
            Err(e) => {
                eprintln!("\n[错误] {e:#}\n");
                messages.pop();
            }
        }
    }

    println!("再见。");
    Ok(())
}
