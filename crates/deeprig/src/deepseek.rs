//! DeepSeek V4 chat client — streaming SSE with usage reporting.
//!
//! Ch08 additions on top of Ch07:
//! - opt into `stream_options: { include_usage: true }` so DeepSeek tells us
//!   how many tokens each turn cost
//! - parse the `usage` field from whichever SSE chunk carries it (typically
//!   the last one before `[DONE]`, with an empty `choices` array)
//! - return both the assistant message AND the usage in a [`ChatOutcome`]
//!
//! Chapter 13 will move this whole module into `deeprig-core` behind the
//! `Provider` trait.

use anyhow::{bail, Context, Result};
use deeprig_core::{Message, TokenUsage};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;

/// DeepSeek's OpenAI-compatible chat completions endpoint.
const API_URL: &str = "https://api.deepseek.com/v1/chat/completions";

/// SSE separator between events: a blank line (`\n\n`).
const SSE_EVENT_SEP: &[u8] = b"\n\n";

/// A single delta emitted while [`chat_stream`] is running.
pub enum StreamEvent {
    /// A chunk of the model's reasoning ("thinking" phase, V4 only).
    Reasoning(String),
    /// A chunk of the model's final answer.
    Content(String),
}

/// Everything [`chat_stream`] hands back to the caller after the stream ends.
pub struct ChatOutcome {
    /// The accumulated assistant message (append to the conversation).
    pub message: Message,
    /// Token usage for this turn — `None` if DeepSeek did not report it.
    pub usage: Option<TokenUsage>,
}

/// One SSE chunk's JSON shape. `choices` and `usage` are both optional:
/// content chunks have `choices` but no `usage`; the final chunk usually has
/// empty `choices` but carries the `usage` object.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: Delta,
}

/// The interesting bits of a streamed delta.
#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Streams a chat completion from DeepSeek.
///
/// `on_event` is called once per delta (reasoning or content) as it arrives.
/// Returns the accumulated assistant [`Message`] and the per-turn usage.
pub async fn chat_stream(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    messages: &[Message],
    mut on_event: impl FnMut(StreamEvent),
) -> Result<ChatOutcome> {
    let response = client
        .post(API_URL)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "messages": messages,
            "stream": true,
            // Ch08: opt in to usage reporting on the final stream chunk.
            "stream_options": { "include_usage": true },
        }))
        .send()
        .await
        .context("HTTP request to DeepSeek failed")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("DeepSeek returned {status}:\n{body}");
    }

    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut content_acc = String::new();
    let mut last_usage: Option<TokenUsage> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("error reading SSE stream")?;
        buf.extend_from_slice(&bytes);

        while let Some(idx) = find_separator(&buf) {
            let event: Vec<u8> = buf.drain(..idx + SSE_EVENT_SEP.len()).collect();
            let text = String::from_utf8_lossy(&event);

            for line in text.lines() {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                if payload.trim() == "[DONE]" {
                    return Ok(ChatOutcome {
                        message: Message::assistant(content_acc),
                        usage: last_usage,
                    });
                }

                let parsed = parse_chunk(payload)?;
                if let Some(u) = parsed.usage {
                    last_usage = Some(u);
                }
                for choice in parsed.choices {
                    if let Some(text) = choice.delta.reasoning_content {
                        on_event(StreamEvent::Reasoning(text));
                    }
                    if let Some(text) = choice.delta.content {
                        content_acc.push_str(&text);
                        on_event(StreamEvent::Content(text));
                    }
                }
            }
        }
    }

    // Stream ended without an explicit `[DONE]` — return what we have.
    Ok(ChatOutcome {
        message: Message::assistant(content_acc),
        usage: last_usage,
    })
}

/// Locates the first SSE event separator (`\n\n`) in `buf`.
fn find_separator(buf: &[u8]) -> Option<usize> {
    buf.windows(SSE_EVENT_SEP.len())
        .position(|w| w == SSE_EVENT_SEP)
}

/// Parses one `data: …` payload (the JSON, without the prefix) into a chunk.
fn parse_chunk(payload: &str) -> Result<StreamChunk> {
    serde_json::from_str(payload).context("SSE chunk JSON did not match the expected shape")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_found_at_correct_index() {
        assert_eq!(find_separator(b"abc\n\ndef"), Some(3));
        assert_eq!(find_separator(b"no separator here"), None);
        assert_eq!(find_separator(b"\n\nat start"), Some(0));
    }

    #[test]
    fn parses_a_content_chunk() {
        let payload = r#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        assert_eq!(chunk.choices.len(), 1);
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn parses_a_reasoning_chunk() {
        let payload = r#"{"choices":[{"delta":{"reasoning_content":"first..."}}]}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("first...")
        );
    }

    #[test]
    fn parses_a_usage_only_chunk() {
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":15,"completion_tokens":20,"total_tokens":35}}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        assert!(chunk.choices.is_empty());
        let u = chunk.usage.expect("usage present");
        assert_eq!(u.prompt_tokens, 15);
        assert_eq!(u.completion_tokens, 20);
        assert_eq!(u.total_tokens, 35);
    }

    #[test]
    fn parses_empty_choices_without_usage() {
        let payload = r#"{"choices":[]}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        assert!(chunk.choices.is_empty());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn malformed_payload_is_an_error() {
        assert!(parse_chunk("not json").is_err());
    }
}
