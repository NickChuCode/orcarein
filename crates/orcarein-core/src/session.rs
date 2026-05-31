//! Multi-turn conversation state.
//!
//! `Session` owns the running `Vec<Message>` plus cumulative token usage.
//! It is intentionally minimal in Ch08 — Ch15 will add persistence
//! (save/load to disk) and Ch13 may tie trimming policies to the
//! `Provider`'s context window.

use crate::Message;
use serde::{Deserialize, Serialize};

/// Token counts reported by the model for one turn.
///
/// Field names match DeepSeek's `usage` object (which mirrors OpenAI's).
/// Capturing this lets us show the user how much budget the conversation is
/// burning before the context window runs out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Adds another usage report into this one, field-wise.
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// One running chat conversation.
///
/// Holds the message history and the cumulative token usage. Methods
/// guarantee the system prompt is never lost (even via `pop_last()` or
/// `clear()`).
pub struct Session {
    messages: Vec<Message>,
    /// The original system message — kept so `clear()` can re-seat it.
    system: Message,
    /// Sum of usage across every successful turn this session has made.
    usage: TokenUsage,
}

impl Session {
    /// Starts a new session with the given system prompt.
    pub fn new(system_prompt: impl Into<String>) -> Self {
        let system = Message::system(system_prompt);
        Session {
            messages: vec![system.clone()],
            system,
            usage: TokenUsage::default(),
        }
    }

    /// The whole running conversation, ready to send to the model.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Appends a user turn.
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(Message::user(content));
    }

    /// Appends an assistant turn.
    pub fn push_assistant(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// Removes the most recent non-system message — used to undo a failed
    /// turn when the model never produced a reply.
    ///
    /// Returns the popped message, or `None` if the only message left is the
    /// system prompt (which we refuse to drop).
    pub fn pop_last(&mut self) -> Option<Message> {
        if self.messages.len() <= 1 {
            return None;
        }
        self.messages.pop()
    }

    /// Resets the conversation to just the system prompt.
    /// Cumulative usage is preserved across clears — it counts spend, not
    /// active context.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.messages.push(self.system.clone());
    }

    /// Cumulative usage across every successful turn this session has made.
    pub fn usage(&self) -> TokenUsage {
        self.usage
    }

    /// Adds a turn's usage to the running total.
    pub fn record_usage(&mut self, u: TokenUsage) {
        self.usage.add(u);
    }

    /// Number of user+assistant messages (excludes the system prompt).
    pub fn turn_count(&self) -> usize {
        self.messages.len().saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_with_only_the_system_prompt() {
        let s = Session::new("be helpful");
        assert_eq!(s.messages().len(), 1);
        assert_eq!(s.messages()[0].role, "system");
        assert_eq!(s.messages()[0].content, "be helpful");
        assert_eq!(s.turn_count(), 0);
    }

    #[test]
    fn push_user_then_assistant_grows_conversation() {
        let mut s = Session::new("sys");
        s.push_user("hi");
        s.push_assistant(Message::assistant("hello"));
        assert_eq!(s.messages().len(), 3);
        assert_eq!(s.messages()[1].role, "user");
        assert_eq!(s.messages()[2].role, "assistant");
        assert_eq!(s.turn_count(), 2);
    }

    #[test]
    fn pop_last_undoes_a_user_turn() {
        let mut s = Session::new("sys");
        s.push_user("hi");
        let popped = s.pop_last().expect("had a user turn to pop");
        assert_eq!(popped.content, "hi");
        assert_eq!(s.messages().len(), 1);
    }

    #[test]
    fn pop_last_refuses_to_drop_the_system_prompt() {
        let mut s = Session::new("sys");
        assert!(s.pop_last().is_none());
        assert_eq!(s.messages().len(), 1);
    }

    #[test]
    fn clear_keeps_system_drops_the_rest() {
        let mut s = Session::new("sys");
        s.push_user("hi");
        s.push_assistant(Message::assistant("hello"));
        s.clear();
        assert_eq!(s.messages().len(), 1);
        assert_eq!(s.messages()[0].role, "system");
        assert_eq!(s.messages()[0].content, "sys");
    }

    #[test]
    fn clear_preserves_cumulative_usage() {
        let mut s = Session::new("sys");
        s.record_usage(TokenUsage {
            prompt_tokens: 5,
            completion_tokens: 3,
            total_tokens: 8,
        });
        s.clear();
        assert_eq!(s.usage().total_tokens, 8);
    }

    #[test]
    fn usage_accumulates_across_turns() {
        let mut s = Session::new("sys");
        s.record_usage(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        s.record_usage(TokenUsage {
            prompt_tokens: 20,
            completion_tokens: 8,
            total_tokens: 28,
        });
        let total = s.usage();
        assert_eq!(total.prompt_tokens, 30);
        assert_eq!(total.completion_tokens, 13);
        assert_eq!(total.total_tokens, 43);
    }

    #[test]
    fn token_usage_round_trips_through_json() {
        let u = TokenUsage {
            prompt_tokens: 15,
            completion_tokens: 20,
            total_tokens: 35,
        };
        let s = serde_json::to_string(&u).unwrap();
        let parsed: TokenUsage = serde_json::from_str(&s).unwrap();
        assert_eq!(u, parsed);
    }
}
