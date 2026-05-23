//! DeepSeek V4 chat client — streaming SSE (Ch07 supersedes Ch06's `chat`).
//!
//! Why streaming: long replies feel frozen if we wait for the whole body.
//! V4 is a reasoning model — it can spend hundreds of ms "thinking" before
//! emitting the first content token. The user needs to *see* something
//! happening, both for UX and for the option to cancel mid-stream.
//!
//! Chapter 13 will move this into `deeprig-core` behind the `Provider` trait.

use anyhow::{bail, Context, Result};
use deeprig_core::Message;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;

/// DeepSeek's OpenAI-compatible chat completions endpoint.
const API_URL: &str = "https://api.deepseek.com/v1/chat/completions";

/// SSE separator between events: a blank line (`\n\n`).
const SSE_EVENT_SEP: &[u8] = b"\n\n";

/// A single delta emitted while [`chat_stream`] is running.
///
/// The caller renders these as they arrive — that is the typewriter effect.
pub enum StreamEvent {
    /// A chunk of the model's reasoning ("thinking" phase, V4 only).
    Reasoning(String),
    /// A chunk of the model's final answer (what gets sent back next turn).
    Content(String),
}

/// One SSE chunk's JSON shape, e.g.
/// `{"choices":[{"delta":{"content":"hi"}}]}`.
#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
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
/// Returns the accumulated assistant [`Message`] — append it to the
/// conversation for the next turn. Reasoning is *not* accumulated in the
/// return value; the caller already saw every chunk via `on_event` and the
/// reasoning text is not part of the conversation context anyway.
pub async fn chat_stream(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    messages: &[Message],
    mut on_event: impl FnMut(StreamEvent),
) -> Result<Message> {
    let response = client
        .post(API_URL)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "messages": messages,
            "stream": true,
        }))
        .send()
        .await
        .context("HTTP request to DeepSeek failed")?;

    let status = response.status();
    if !status.is_success() {
        // Error responses are short — read the whole body, no need to stream.
        let body = response.text().await.unwrap_or_default();
        bail!("DeepSeek returned {status}:\n{body}");
    }

    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut content_acc = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("error reading SSE stream")?;
        buf.extend_from_slice(&bytes);

        // Process every complete event currently in the buffer. An event is
        // bytes up to and including the next `\n\n` separator.
        while let Some(idx) = find_separator(&buf) {
            let event: Vec<u8> = buf.drain(..idx + SSE_EVENT_SEP.len()).collect();
            let text = String::from_utf8_lossy(&event);

            for line in text.lines() {
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue; // skip comments, empty lines, `event:` lines
                };
                if payload.trim() == "[DONE]" {
                    return Ok(Message::assistant(content_acc));
                }

                let delta = parse_delta(payload)?;
                if let Some(text) = delta.reasoning_content {
                    on_event(StreamEvent::Reasoning(text));
                }
                if let Some(text) = delta.content {
                    content_acc.push_str(&text);
                    on_event(StreamEvent::Content(text));
                }
            }
        }
    }

    // Stream ended without an explicit `[DONE]` marker — treat as if it had.
    Ok(Message::assistant(content_acc))
}

/// Locates the first SSE event separator (`\n\n`) in `buf`.
fn find_separator(buf: &[u8]) -> Option<usize> {
    buf.windows(SSE_EVENT_SEP.len())
        .position(|w| w == SSE_EVENT_SEP)
}

/// Parses the JSON payload of one `data: …` SSE line into a [`Delta`].
fn parse_delta(payload: &str) -> Result<Delta> {
    let chunk: StreamChunk =
        serde_json::from_str(payload).context("SSE chunk JSON did not match the expected shape")?;
    Ok(chunk
        .choices
        .into_iter()
        .next()
        .map(|c| c.delta)
        .unwrap_or_default())
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
    fn parses_a_content_delta() {
        let payload = r#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        let delta = parse_delta(payload).expect("should parse");
        assert_eq!(delta.content.as_deref(), Some("hi"));
        assert!(delta.reasoning_content.is_none());
    }

    #[test]
    fn parses_a_reasoning_delta() {
        let payload = r#"{"choices":[{"delta":{"reasoning_content":"first..."}}]}"#;
        let delta = parse_delta(payload).expect("should parse");
        assert_eq!(delta.reasoning_content.as_deref(), Some("first..."));
        assert!(delta.content.is_none());
    }

    #[test]
    fn parses_a_delta_with_both_fields() {
        let payload = r#"{"choices":[{"delta":{"content":"yes","reasoning_content":"because"}}]}"#;
        let delta = parse_delta(payload).expect("should parse");
        assert_eq!(delta.content.as_deref(), Some("yes"));
        assert_eq!(delta.reasoning_content.as_deref(), Some("because"));
    }

    #[test]
    fn parses_empty_delta_as_default() {
        let payload = r#"{"choices":[{"delta":{}}]}"#;
        let delta = parse_delta(payload).expect("should parse");
        assert!(delta.content.is_none());
        assert!(delta.reasoning_content.is_none());
    }

    #[test]
    fn malformed_payload_is_an_error() {
        assert!(parse_delta("not json").is_err());
    }
}
