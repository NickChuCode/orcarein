//! DeepRig CLI — interactive chat REPL.
//!
//! Chapter 6 milestone: a multi-turn conversation loop. The binary reads a
//! line, sends the whole conversation to DeepSeek, prints the reply, and
//! repeats. A failed request prints an error and keeps the loop alive.
//!
//! Still missing (later chapters): streaming output (Ch07), tools (Ch09+).

mod deepseek;

use anyhow::{Context, Result};
use deeprig_core::Message;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

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

    // The running conversation. The system message goes first; user and
    // assistant messages accumulate so the model sees full context each turn.
    let mut messages = vec![Message::system(SYSTEM_PROMPT)];

    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;

    println!("DeepRig — chat with {model}. Ctrl+D or /exit to quit.\n");

    loop {
        // `readline` blocks until the user presses Enter. Blocking inside an
        // async fn is fine here: there is nothing else to do while we wait.
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue, // Ctrl+C: discard input
            Err(ReadlineError::Eof) => break,            // Ctrl+D: quit
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

        // A failed request must NOT crash the REPL. We `match` the `Result`
        // instead of using `?`: `?` would propagate the error out of `main`
        // and end the program. In a loop, we want to recover and continue.
        match deepseek::chat(&client, &api_key, &model, &messages).await {
            Ok(reply) => {
                if let Some(reasoning) = &reply.reasoning {
                    if !reasoning.is_empty() {
                        println!("[思考]\n{reasoning}\n");
                    }
                }
                println!("[回复]\n{}\n", reply.message.content);
                messages.push(reply.message);
            }
            Err(e) => {
                // `{e:#}` prints the whole error chain — every `.context()`
                // layer, joined with ": ".
                eprintln!("[错误] {e:#}\n");
                // The user's turn got no reply. Drop it so the next request
                // is not two `user` messages in a row.
                messages.pop();
            }
        }
    }

    println!("再见。");
    Ok(())
}
