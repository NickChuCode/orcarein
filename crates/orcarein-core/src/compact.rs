//! Manual context compaction (`/compact`): summarize older turns into one
//! message, keep the system prompt + recent turns verbatim. See the v02-21
//! design spec. Pure helpers here; the binary drives them from `/compact`.

use crate::Message;

/// How many user-anchored exchanges to keep verbatim (configurable later).
pub const KEEP_RECENT_USER_TURNS: usize = 2;

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
}
