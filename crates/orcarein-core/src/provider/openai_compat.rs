//! Shared OpenAI-compatible Chat Completions streaming implementation.
//!
//! DeepSeek and OpenAI publish wire-compatible SSE endpoints. The
//! request body, the `data: …\n\n` framing, the `tool_calls[i]` delta
//! shape, and the `[DONE]` sentinel all match. The only differences
//! are the endpoint URL, the bearer token, and the default model.
//!
//! `DeepSeekProvider` and `OpenAIProvider` both delegate here.
//! Anthropic — when it eventually arrives in v0.2 — would get its own
//! parser because its protocol is not OpenAI-compatible.

use anyhow::{Context, Result};
use async_stream::try_stream;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

use super::retry::{
    classify_reqwest, classify_status, with_retry, Decision, RetryError, RetryPolicy,
};
use super::{ChatOptions, StreamEvent};
use crate::{FunctionCall, Message, TokenUsage, ToolCall, ToolDefinition};

/// SSE separator between events: a blank line (`\n\n`).
const SSE_EVENT_SEP: &[u8] = b"\n\n";

/// Issues a POST to an OpenAI-compatible `chat/completions` endpoint and
/// returns a streaming response in the form of `StreamEvent`s.
pub(super) async fn chat_stream_compat(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: &str,
    policy: &RetryPolicy,
    messages: &[Message],
    tools: &[ToolDefinition],
    opts: &ChatOptions,
) -> Result<BoxStream<'static, Result<StreamEvent>>> {
    let mut body = json!({
        "model": opts.model,
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

    let response = with_retry(policy, || async {
        let sent = client
            .post(endpoint)
            .bearer_auth(api_key)
            .json(&body)
            .send();
        // Bound connect + response headers only. send() resolves at the
        // headers, so this does NOT cap the SSE body that streams afterwards.
        match tokio::time::timeout(policy.request_timeout, sent).await {
            Err(_elapsed) => Err(RetryError::Retryable {
                source: anyhow::anyhow!("request timed out before response headers"),
                retry_after: None,
            }),
            Ok(Err(e)) => {
                // Classify borrows `e`; then `e` moves into Error::new (borrow
                // ends first — this order is required).
                let decision = classify_reqwest(&e);
                let err = anyhow::Error::new(e).context("HTTP request failed");
                Err(match decision {
                    Decision::Retryable(ra) => RetryError::Retryable {
                        source: err,
                        retry_after: ra,
                    },
                    Decision::Fatal => RetryError::Fatal(err),
                })
            }
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(resp)
                } else {
                    let decision = classify_status(status, resp.headers());
                    let body = tokio::time::timeout(policy.request_timeout, resp.text())
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                    let err = anyhow::anyhow!("provider returned {status}:\n{body}");
                    Err(match decision {
                        Decision::Retryable(ra) => RetryError::Retryable {
                            source: err,
                            retry_after: ra,
                        },
                        Decision::Fatal => RetryError::Fatal(err),
                    })
                }
            }
        }
    })
    .await?;

    let bytes_stream = response.bytes_stream();

    Ok(Box::pin(try_stream! {
        let mut bytes_stream = bytes_stream;
        let mut buf: Vec<u8> = Vec::new();
        let mut tool_acc = ToolCallAcc::new();
        let mut last_usage: Option<TokenUsage> = None;

        while let Some(chunk) = bytes_stream.next().await {
            let bytes = chunk.context("error reading SSE stream")?;
            buf.extend_from_slice(&bytes);

            while let Some(idx) = find_separator(&buf) {
                let event: Vec<u8> = buf.drain(..idx + SSE_EVENT_SEP.len()).collect();
                let text = String::from_utf8_lossy(&event);

                for line in text.lines() {
                    let Some(payload) = line.strip_prefix("data: ") else { continue };
                    if payload.trim() == "[DONE]" { continue; }

                    let parsed = parse_chunk(payload)?;
                    if let Some(u) = parsed.usage { last_usage = Some(u); }
                    for choice in parsed.choices {
                        if let Some(text) = choice.delta.reasoning_content {
                            yield StreamEvent::Reasoning(text);
                        }
                        if let Some(text) = choice.delta.content {
                            yield StreamEvent::Content(text);
                        }
                        for d in choice.delta.tool_calls {
                            tool_acc.merge(d);
                        }
                    }
                }
            }
        }

        let calls = tool_acc.finalize();
        if !calls.is_empty() { yield StreamEvent::ToolCalls(calls); }
        if let Some(u) = last_usage { yield StreamEvent::Usage(u); }
    }))
}

// ---------------------------------------------------------------------------
// SSE chunk types (deserialize-only)
// ---------------------------------------------------------------------------

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

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Parse an OpenAI-compatible `/v1/models` body into sorted, deduped model ids.
/// Pure (HTTP-free) so it is unit-tested without the network.
pub(super) fn parse_models(body: &str) -> Result<Vec<String>> {
    let resp: ModelsResponse = serde_json::from_str(body).context("parse /v1/models body")?;
    let mut ids: Vec<String> = resp.data.into_iter().map(|m| m.id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// `GET {models_url}` (a full URL) with bearer auth and parse the catalog.
pub(super) async fn list_models_compat(
    client: &reqwest::Client,
    models_url: &str,
    api_key: &str,
    policy: &RetryPolicy,
) -> Result<Vec<String>> {
    let resp = with_retry(policy, || async {
        let sent = client.get(models_url).bearer_auth(api_key).send();
        match tokio::time::timeout(policy.request_timeout, sent).await {
            Err(_elapsed) => Err(RetryError::Retryable {
                source: anyhow::anyhow!("models request timed out before response headers"),
                retry_after: None,
            }),
            Ok(Err(e)) => {
                let decision = classify_reqwest(&e);
                let err = anyhow::Error::new(e).context("models request failed");
                Err(match decision {
                    Decision::Retryable(ra) => RetryError::Retryable {
                        source: err,
                        retry_after: ra,
                    },
                    Decision::Fatal => RetryError::Fatal(err),
                })
            }
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(resp)
                } else {
                    let decision = classify_status(status, resp.headers());
                    let body = tokio::time::timeout(policy.request_timeout, resp.text())
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                    let err = anyhow::anyhow!("models endpoint returned {status}:\n{body}");
                    Err(match decision {
                        Decision::Retryable(ra) => RetryError::Retryable {
                            source: err,
                            retry_after: ra,
                        },
                        Decision::Fatal => RetryError::Fatal(err),
                    })
                }
            }
        }
    })
    .await?;

    let body = resp.text().await.unwrap_or_default();
    parse_models(&body)
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
    fn parse_models_extracts_sorted_deduped_ids() {
        let body = r#"{"data":[{"id":"deepseek-v4-pro"},{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro"}]}"#;
        let got = parse_models(body).unwrap();
        assert_eq!(
            got,
            vec![
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string()
            ]
        );
    }

    #[test]
    fn parse_models_rejects_malformed_body() {
        assert!(parse_models("not json").is_err());
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
