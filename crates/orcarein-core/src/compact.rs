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
}
