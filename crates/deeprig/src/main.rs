//! DeepRig CLI — entry point.
//!
//! Chapter 5 milestone: send one chat request to DeepSeek V4 and print the
//! reply. No streaming, no multi-turn, no tools yet — those arrive in later
//! chapters. The goal here is simply: prove the round trip works.

use anyhow::{bail, Context, Result};
use deeprig_core::Message;
use serde::Deserialize;
use serde_json::json;

/// Model used when none is given on the command line.
///
/// DeepSeek V4 exposes `deepseek-v4-flash` (fast, cheap) and `deepseek-v4-pro`.
/// We never hard-code this at the call site — it stays configurable so a
/// future config file (Ch14) can override it.
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// DeepSeek's OpenAI-compatible chat completions endpoint.
const API_URL: &str = "https://api.deepseek.com/v1/chat/completions";

/// The slice of DeepSeek's response we care about this chapter.
///
/// DeepSeek returns far more fields (`id`, `usage`, `system_fingerprint`, ...).
/// `serde` ignores any field we don't declare, so we only model what we use.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
    /// DeepSeek V4 is a reasoning model: it may return its chain of thought
    /// here, separate from the final `content`.
    ///
    /// `#[serde(default)]` means "if this field is absent, use `Option`'s
    /// default — `None`". Without it, a response lacking `reasoning_content`
    /// would fail to deserialize.
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The API key comes from the environment. It is never written to a file
    // or hard-coded. `.context()` turns the raw VarError into a message that
    // tells the user exactly what to do.
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .context("DEEPSEEK_API_KEY not set. In PowerShell: $env:DEEPSEEK_API_KEY = '<your-key>'")?;

    // Model name: first CLI argument, or the default. Run a different model
    // with: `cargo run -- deepseek-v4-pro`
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MODEL.into());

    // The conversation. For now it is a single user message; Ch06 makes this
    // an interactive, growing list.
    let messages = vec![Message::user("用一句话介绍你自己。")];

    let client = reqwest::Client::new();
    let response = client
        .post(API_URL)
        .bearer_auth(&api_key)
        .json(&json!({
            "model": model,
            "messages": messages,
            "stream": false,
        }))
        .send()
        .await
        .context("HTTP request to DeepSeek failed")?;

    // Read status and body separately so we can show DeepSeek's own error
    // explanation on a 4xx/5xx — `reqwest`'s `error_for_status()` would
    // discard the body, which is exactly where the useful detail lives.
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read the response body")?;
    if !status.is_success() {
        bail!("DeepSeek returned {status}:\n{body}");
    }

    let parsed: ChatResponse =
        serde_json::from_str(&body).context("response JSON did not match the expected shape")?;
    let reply = parsed
        .choices
        .first()
        .context("DeepSeek returned an empty `choices` array")?;

    // V4 may "think" before answering. Show the reasoning if it sent any.
    if let Some(reasoning) = &reply.message.reasoning_content {
        if !reasoning.is_empty() {
            println!("[思考]\n{reasoning}\n");
        }
    }
    println!("[回复]\n{}", reply.message.content);

    Ok(())
}
