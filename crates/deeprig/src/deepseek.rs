//! DeepSeek V4 chat client — streaming SSE with tool-call accumulation.
//!
//! Ch09 additions on top of Ch08:
//! - parse streamed `tool_calls` deltas (`{name,arguments}` arrive piece by
//!   piece, indexed by `tool_calls[i].index`)
//! - accumulate them into complete `ToolCall`s by the time the stream ends
//! - return them on [`ChatOutcome`]
//!
//! Ch10 additions:
//! - `chat_stream` accepts a `tools: &[ToolDefinition]` slice and includes
//!   it (plus `"tool_choice": "auto"`) in the request body. When the
//!   slice is empty the body is byte-identical to Ch09's so existing
//!   no-tools behaviour is preserved.
//!
//! Execution of the returned `tool_calls` lives in the REPL
//! (`main.rs::run_turn`), not here — this module stays a thin provider
//! shim.

use anyhow::{bail, Context, Result};
use deeprig_core::{FunctionCall, Message, TokenUsage, ToolCall, ToolDefinition};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

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

/// Everything [`chat_stream`] returns after the stream ends.
pub struct ChatOutcome {
    /// The accumulated assistant message. Carries `tool_calls` if the model
    /// wanted to use tools instead of (or in addition to) text content.
    pub message: Message,
    /// Token usage for this turn — `None` if DeepSeek did not report it.
    pub usage: Option<TokenUsage>,
}

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

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Tool-call deltas — incremental, indexed by `index`.
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

/// One slice of a streamed tool call. Fields are all optional; the first
/// chunk usually carries `id`/`name`, subsequent chunks add to `arguments`.
#[derive(Deserialize)]
struct ToolCallDelta {
    index: u64,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    function: Option<FunctionCallDelta>,
}

#[derive(Deserialize)]
struct FunctionCallDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Streams a chat completion from DeepSeek.
///
/// `tools` is included in the request body only when non-empty — keeping
/// the no-tools wire shape identical to Ch09.
pub async fn chat_stream(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    mut on_event: impl FnMut(StreamEvent),
) -> Result<ChatOutcome> {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !tools.is_empty() {
        // serde_json::to_value cannot fail for a Vec<ToolDefinition> — all
        // fields are JSON-friendly types — so .expect is fine here.
        body["tools"] = serde_json::to_value(tools).expect("tools serialize");
        body["tool_choice"] = json!("auto");
    }

    let response = client
        .post(API_URL)
        .bearer_auth(api_key)
        .json(&body)
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
    let mut reasoning_acc = String::new();
    let mut tool_acc = ToolCallAcc::new();
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
                    return Ok(finalize(content_acc, reasoning_acc, tool_acc, last_usage));
                }

                let parsed = parse_chunk(payload)?;
                if let Some(u) = parsed.usage {
                    last_usage = Some(u);
                }
                for choice in parsed.choices {
                    if let Some(text) = choice.delta.reasoning_content {
                        // Accumulate AND forward — V4 demands the
                        // assistant `reasoning_content` be echoed back on
                        // the next call (e.g. after a tool result).
                        reasoning_acc.push_str(&text);
                        on_event(StreamEvent::Reasoning(text));
                    }
                    if let Some(text) = choice.delta.content {
                        content_acc.push_str(&text);
                        on_event(StreamEvent::Content(text));
                    }
                    for d in choice.delta.tool_calls {
                        tool_acc.merge(d);
                    }
                }
            }
        }
    }

    Ok(finalize(content_acc, reasoning_acc, tool_acc, last_usage))
}

/// Packages the loop state into a [`ChatOutcome`].
fn finalize(
    content: String,
    reasoning: String,
    tool_acc: ToolCallAcc,
    usage: Option<TokenUsage>,
) -> ChatOutcome {
    let tool_calls = tool_acc.finalize();
    let message = if tool_calls.is_empty() {
        Message::assistant(content)
    } else {
        Message::assistant_with_tool_calls(content, tool_calls)
    };
    let message = message.with_reasoning(reasoning);
    ChatOutcome { message, usage }
}

/// Index of the first SSE event separator (`\n\n`) in `buf`.
fn find_separator(buf: &[u8]) -> Option<usize> {
    buf.windows(SSE_EVENT_SEP.len())
        .position(|w| w == SSE_EVENT_SEP)
}

/// Parses one `data: …` payload into a chunk.
fn parse_chunk(payload: &str) -> Result<StreamChunk> {
    serde_json::from_str(payload).context("SSE chunk JSON did not match the expected shape")
}

// ---------------------------------------------------------------------------
// Tool-call accumulator
// ---------------------------------------------------------------------------

/// A partial tool call being built up across multiple SSE deltas.
#[derive(Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    kind: Option<String>,
}

/// Accumulates streamed `tool_calls[i]` deltas into complete [`ToolCall`]s.
///
/// Indexed by the `index` field — the API uses it to associate fragments
/// belonging to the same call (and to support parallel tool calls).
struct ToolCallAcc {
    calls: BTreeMap<u64, PartialToolCall>,
}

impl ToolCallAcc {
    fn new() -> Self {
        ToolCallAcc {
            calls: BTreeMap::new(),
        }
    }

    fn merge(&mut self, delta: ToolCallDelta) {
        let entry = self.calls.entry(delta.index).or_default();
        if let Some(id) = delta.id {
            entry.id = Some(id);
        }
        if let Some(kind) = delta.kind {
            entry.kind = Some(kind);
        }
        if let Some(func) = delta.function {
            if let Some(name) = func.name {
                entry.name = Some(name);
            }
            if let Some(args) = func.arguments {
                entry.arguments.push_str(&args);
            }
        }
    }

    /// Materializes complete `ToolCall`s. Drops any partial call that never
    /// got an `id` or `name` (which would have been broken anyway).
    fn finalize(self) -> Vec<ToolCall> {
        self.calls
            .into_values()
            .filter_map(|p| {
                Some(ToolCall {
                    id: p.id?,
                    kind: p.kind.unwrap_or_else(|| "function".into()),
                    function: FunctionCall {
                        name: p.name?,
                        arguments: p.arguments,
                    },
                })
            })
            .collect()
    }
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
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
    }

    #[test]
    fn parses_a_usage_only_chunk() {
        let payload = r#"{"choices":[],"usage":{"prompt_tokens":15,"completion_tokens":20,"total_tokens":35}}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        assert_eq!(chunk.usage.unwrap().total_tokens, 35);
    }

    #[test]
    fn parses_a_tool_call_chunk() {
        let payload = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"f","arguments":""}}]}}]}"#;
        let chunk = parse_chunk(payload).expect("should parse");
        let tc = &chunk.choices[0].delta.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_a"));
    }

    #[test]
    fn malformed_payload_is_an_error() {
        assert!(parse_chunk("not json").is_err());
    }

    // -----------------------------------------------------------------------
    // Tool-call accumulator tests
    // -----------------------------------------------------------------------

    fn delta(
        index: u64,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: id.map(String::from),
            kind: id.map(|_| "function".to_string()), // pair with first chunk
            function: Some(FunctionCallDelta {
                name: name.map(String::from),
                arguments: args.map(String::from),
            }),
        }
    }

    #[test]
    fn acc_assembles_a_single_tool_call_from_chunks() {
        let mut acc = ToolCallAcc::new();
        acc.merge(delta(0, Some("call_1"), Some("get_weather"), Some("")));
        acc.merge(delta(0, None, None, Some(r#"{"city":"#)));
        acc.merge(delta(0, None, None, Some(r#""Tokyo"}"#)));
        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(
            calls[0].function.parse_arguments().unwrap()["city"],
            "Tokyo"
        );
    }

    #[test]
    fn acc_assembles_parallel_tool_calls_by_index() {
        let mut acc = ToolCallAcc::new();
        acc.merge(delta(0, Some("call_a"), Some("f"), Some("{}")));
        acc.merge(delta(1, Some("call_b"), Some("g"), Some("{}")));
        let calls = acc.finalize();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a"); // BTreeMap → ordered by index
        assert_eq!(calls[1].id, "call_b");
    }

    #[test]
    fn acc_drops_calls_missing_id_or_name() {
        // A delta that arrived in pieces but never got an id is broken;
        // finalize must not produce a malformed `ToolCall`.
        let mut acc = ToolCallAcc::new();
        acc.merge(delta(0, None, Some("f"), Some("{}")));
        assert!(acc.finalize().is_empty());
    }
}
