//! Manual context compaction (`/compact`): summarize older turns into one
//! message, keep the system prompt + recent turns verbatim. See the v02-21
//! design spec. Pure helpers here; the binary drives them from `/compact`.

use crate::{ChatOptions, Message, Provider, Session, StreamEvent, TokenUsage};
use futures_util::StreamExt;

/// How many user-anchored exchanges to keep verbatim (configurable later).
pub const KEEP_RECENT_USER_TURNS: usize = 2;

const SUMMARY_INSTRUCTION: &str = "You are compacting a coding-assistant conversation. \
    Summarize the transcript that follows into a concise note that preserves: decisions made, \
    file paths touched, the current task state, and open questions. Omit chit-chat. Output only \
    the summary.";

/// Result of a `/compact` attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    Compacted {
        messages_before: usize,
        messages_after: usize,
        chars_before: usize,
        chars_after: usize,
    },
    NothingToDo,
}

/// Errors from [`compact_session`].
#[derive(Debug, thiserror::Error)]
pub enum CompactError {
    #[error("summarization request failed: {0}")]
    Provider(#[from] anyhow::Error),
    #[error("model returned an empty summary")]
    EmptySummary,
}

fn total_chars(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.content.len()).sum()
}

/// Summarize the older span and rewrite `session` in place. One tool-less
/// provider call. On any failure the session is left untouched.
pub async fn compact_session(
    session: &mut Session,
    provider: &dyn Provider,
    model: &str,
    keep: usize,
) -> Result<CompactOutcome, CompactError> {
    let boundary = match compaction_boundary(session.messages(), keep) {
        Some(b) => b,
        None => return Ok(CompactOutcome::NothingToDo),
    };
    let messages_before = session.messages().len();
    let chars_before = total_chars(session.messages());

    // Flatten the old span to plain text — no reasoning/tool-chain replay.
    let span_text = render_span(&session.messages()[1..boundary]);
    let summarize = vec![Message::system(SUMMARY_INSTRUCTION), Message::user(span_text)];
    let opts = ChatOptions::new(model);

    let mut stream = provider.chat_stream(&summarize, &[], &opts).await?;
    let mut summary = String::new();
    let mut usage: Option<TokenUsage> = None;
    while let Some(ev) = stream.next().await {
        match ev? {
            StreamEvent::Content(c) => summary.push_str(&c),
            StreamEvent::Usage(u) => usage = Some(u),
            _ => {}
        }
    }
    if summary.trim().is_empty() {
        return Err(CompactError::EmptySummary); // session untouched, usage not recorded
    }

    session.compact_at(boundary, &summary);
    if let Some(u) = usage {
        session.record_usage(u); // record only on success
    }
    Ok(CompactOutcome::Compacted {
        messages_before,
        messages_after: session.messages().len(),
        chars_before,
        chars_after: total_chars(session.messages()),
    })
}

/// Index of the verbatim cutoff: the `keep`-th-from-last `user` message.
/// Everything in `messages[1..boundary]` gets summarized; `[0]` (system) and
/// `[boundary..]` stay verbatim. `None` when there is not enough to compact.
/// Choosing a `user` index guarantees the cut never orphans a tool result.
pub fn compaction_boundary(messages: &[Message], keep: usize) -> Option<usize> {
    let user_idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if user_idxs.len() <= keep {
        return None;
    }
    // The k-th-from-last user index. Since messages[0] is always the system
    // prompt, user indices are >= 1 and strictly increasing, so with
    // user_idxs.len() > keep this is always >= 2 — never the degenerate
    // `[system, summary]` (no-recent) case.
    Some(user_idxs[user_idxs.len() - keep])
}

/// Flattens a message span to plain text for the summary call. Deliberately
/// drops `reasoning_content` and never replays structured `tool_calls`/`tool`
/// messages (replaying either can 400 DeepSeek V4 / OpenAI-compatible APIs).
pub fn render_span(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role.as_str() {
            "assistant" if !m.tool_calls.is_empty() => {
                let names: Vec<&str> =
                    m.tool_calls.iter().map(|c| c.function.name.as_str()).collect();
                let tools = names.join(", ");
                if m.content.is_empty() {
                    out.push_str(&format!("assistant: [called tool: {tools}]"));
                } else {
                    out.push_str(&format!("assistant: {} [called tool: {tools}]", m.content));
                }
            }
            "tool" => {
                let id = m.tool_call_id.as_deref().unwrap_or("");
                out.push_str(&format!("tool({id}): {}", m.content));
            }
            other => out.push_str(&format!("{other}: {}", m.content)),
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str) -> Message {
        match role {
            "user" => Message::user("u"),
            "assistant" => Message::assistant("a"),
            _ => Message::system("s"),
        }
    }

    #[test]
    fn boundary_keeps_last_two_user_blocks() {
        // [sys,u,a,u,a,u,a] users at 1,3,5; keep=2 -> boundary 3.
        let m: Vec<Message> =
            ["system", "user", "assistant", "user", "assistant", "user", "assistant"]
                .iter()
                .map(|r| msg(r))
                .collect();
        assert_eq!(compaction_boundary(&m, 2), Some(3));
        assert_eq!(m[3].role, "user");
    }

    #[test]
    fn boundary_none_when_history_short() {
        let short: Vec<Message> =
            ["system", "user", "assistant"].iter().map(|r| msg(r)).collect();
        assert_eq!(compaction_boundary(&short, 2), None);
        // Exactly `keep` user turns -> still None (nothing older to fold).
        let exactly: Vec<Message> = ["system", "user", "assistant", "user", "assistant"]
            .iter()
            .map(|r| msg(r))
            .collect();
        assert_eq!(compaction_boundary(&exactly, 2), None);
    }

    use crate::{FunctionCall, ToolCall};

    fn asst_with_tool(content: &str, tool: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: content.into(),
            reasoning_content: Some("SECRET THINKING".into()),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: tool.into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        }
    }

    #[test]
    fn render_span_flattens_without_reasoning() {
        let span = vec![
            Message::user("hello"),
            asst_with_tool("", "search"),                // tool-only
            asst_with_tool("thinking out loud", "edit"), // content + tool
        ];
        let out = super::render_span(&span);
        assert!(out.contains("user: hello"));
        assert!(out.contains("assistant: [called tool: search]"));
        assert!(out.contains("assistant: thinking out loud [called tool: edit]"));
        // reasoning_content must NEVER leak into the summary input (V4 400 risk).
        assert!(!out.contains("SECRET THINKING"));
    }

    use crate::{MockProvider, Session, StreamEvent, TokenUsage};

    fn session_with_turns(n: usize) -> Session {
        let mut s = Session::new("SYS");
        for i in 0..n {
            s.push_user(format!("u{i}"));
            s.push_assistant(Message::assistant(format!("a{i}")));
        }
        s
    }

    #[tokio::test]
    async fn compact_session_summarizes_and_rewrites() {
        let mut s = session_with_turns(5); // [sys, u0,a0,...,u4,a4]
        let before = s.messages().len();
        let provider = MockProvider::new();
        provider.push_response(vec![
            StreamEvent::Content("the summary".into()),
            StreamEvent::Usage(TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 10,
                total_tokens: 110,
                ..Default::default()
            }),
        ]);

        let outcome = super::compact_session(&mut s, &provider, "mock-model", 2)
            .await
            .unwrap();
        match outcome {
            super::CompactOutcome::Compacted {
                messages_before,
                messages_after,
                ..
            } => {
                assert_eq!(messages_before, before);
                assert!(messages_after < before);
            }
            other => panic!("expected Compacted, got {other:?}"),
        }
        // [system, summary(user), u3, a3, u4, a4]
        assert_eq!(s.messages()[1].role, "user");
        assert!(s.messages()[1].content.contains("the summary"));
        // The summary call's tokens were recorded.
        assert_eq!(s.usage().total_tokens, 110);
    }

    #[tokio::test]
    async fn compact_session_nothing_to_do_makes_no_call() {
        let mut s = session_with_turns(1); // too short
        let provider = MockProvider::new();
        provider.push_text("unused");
        let outcome = super::compact_session(&mut s, &provider, "mock-model", 2)
            .await
            .unwrap();
        assert!(matches!(outcome, super::CompactOutcome::NothingToDo));
        assert_eq!(provider.pending(), 1, "provider must not be called");
    }

    #[tokio::test]
    async fn compact_session_empty_summary_errors_and_leaves_session() {
        let mut s = session_with_turns(5);
        let snapshot = s.messages().to_vec();
        let provider = MockProvider::new();
        provider.push_text("   "); // whitespace-only summary
        let err = super::compact_session(&mut s, &provider, "mock-model", 2).await;
        assert!(err.is_err());
        assert_eq!(
            s.messages(),
            snapshot.as_slice(),
            "session unchanged on empty summary"
        );
        assert_eq!(s.usage().total_tokens, 0, "no usage recorded on failure");
    }
}
