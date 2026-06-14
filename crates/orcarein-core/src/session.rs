//! Multi-turn conversation state.
//!
//! `Session` owns the running `Vec<Message>` plus cumulative token usage.
//! It is intentionally minimal in Ch08 — Ch15 will add persistence
//! (save/load to disk) and Ch13 may tie trimming policies to the
//! `Provider`'s context window.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Prompt tokens served from DeepSeek's automatic byte-prefix cache (the
    /// cheap ones). DeepSeek reports this in `usage`; OpenAI does not, so it
    /// stays 0 there. `prompt_cache_hit_tokens + prompt_cache_miss_tokens`
    /// equals `prompt_tokens` on DeepSeek.
    #[serde(default)]
    pub prompt_cache_hit_tokens: u64,
    /// Prompt tokens NOT served from cache (full price). See above.
    #[serde(default)]
    pub prompt_cache_miss_tokens: u64,
}

impl TokenUsage {
    /// Adds another usage report into this one, field-wise.
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.prompt_cache_hit_tokens += other.prompt_cache_hit_tokens;
        self.prompt_cache_miss_tokens += other.prompt_cache_miss_tokens;
    }
}

/// One running chat conversation.
///
/// Holds the message history and the cumulative token usage. Methods
/// guarantee the system prompt is never lost (even via `pop_last()` or
/// `clear()`).
///
/// Ch15: derives `Serialize`/`Deserialize` so a session can be saved to and
/// resumed from a JSON file (see [`SessionStore`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Errors from saving or loading a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed session JSON: {0}")]
    ParseJson(#[from] serde_json::Error),

    #[error("could not locate a data directory for this platform")]
    NoDataDir,
}

/// On-disk envelope: the session plus its creation timestamp. The id lives in
/// the filename, so it is not duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionFile {
    created_at_ms: u64,
    session: Session,
}

/// One row of `session list` — enough to identify a saved session at a glance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub created_at_ms: u64,
    pub turns: usize,
    /// First user message, trimmed to a short single-line title.
    pub title: String,
}

/// Persists sessions as `<dir>/<id>.json`.
///
/// Session files are *data*, not configuration, so the default directory comes
/// from `ProjectDirs::data_dir()` (`~/.local/share/orcarein/sessions` on Linux,
/// `%APPDATA%\orcarein\data\sessions` on Windows) — not `config_dir()`.
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// The default sessions directory (`<data_dir>/sessions`).
    pub fn sessions_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "orcarein").map(|d| d.data_dir().join("sessions"))
    }

    /// A store rooted at the platform data directory.
    pub fn new() -> Result<Self, SessionError> {
        let dir = Self::sessions_dir().ok_or(SessionError::NoDataDir)?;
        Ok(Self { dir })
    }

    /// A store rooted at an explicit directory (used by tests).
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory this store reads and writes.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The on-disk path for a session id.
    pub fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// The current wall-clock time as Unix-epoch milliseconds. A clock set
    /// before 1970 collapses to 0 rather than panicking.
    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// A fresh, time-ordered session id (Unix-epoch milliseconds as a string).
    pub fn new_id() -> String {
        Self::now_ms().to_string()
    }

    /// Writes `session` to `<dir>/<id>.json`, stamping `created_at_ms`. The
    /// caller passes the creation time so re-saving a resumed session keeps its
    /// original timestamp instead of bumping it every turn.
    pub fn save(
        &self,
        id: &str,
        created_at_ms: u64,
        session: &Session,
    ) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let file = SessionFile {
            created_at_ms,
            session: session.clone(),
        };
        std::fs::write(self.path_for(id), serde_json::to_string_pretty(&file)?)?;
        Ok(())
    }

    /// Reads a session back by id.
    pub fn load(&self, id: &str) -> Result<Session, SessionError> {
        Ok(self.read_file(id)?.session)
    }

    /// Deletes the session file for `id`. A missing id surfaces as an `Io`
    /// NotFound error so the caller can report it (auto-save only ever grows
    /// the directory; this is the explicit prune the user reaches for).
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        std::fs::remove_file(self.path_for(id))?;
        Ok(())
    }

    /// The stored creation timestamp for `id` (so a resumed session can be
    /// re-saved under its original time).
    pub fn created_at(&self, id: &str) -> Result<u64, SessionError> {
        Ok(self.read_file(id)?.created_at_ms)
    }

    fn read_file(&self, id: &str) -> Result<SessionFile, SessionError> {
        let text = std::fs::read_to_string(self.path_for(id))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Summaries of every saved session, newest first. A missing directory
    /// yields an empty list; an unreadable/corrupt file is skipped rather than
    /// failing the whole listing.
    pub fn list(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = serde_json::from_str::<SessionFile>(&text) else {
                continue;
            };
            out.push(SessionSummary {
                id: id.to_owned(),
                created_at_ms: file.created_at_ms,
                turns: file.session.turn_count(),
                title: summarize_title(&file.session),
            });
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.created_at_ms));
        Ok(out)
    }
}

/// First user message, trimmed to a short single-line title for `session list`.
fn summarize_title(session: &Session) -> String {
    const MAX: usize = 60;
    let Some(msg) = session.messages().iter().find(|m| m.role == "user") else {
        return "(no user message)".to_owned();
    };
    let line = msg.content.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        "(empty)".to_owned()
    } else if line.chars().count() > MAX {
        let head: String = line.chars().take(MAX).collect();
        format!("{head}…")
    } else {
        line.to_owned()
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
            ..Default::default()
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
            ..Default::default()
        });
        s.record_usage(TokenUsage {
            prompt_tokens: 20,
            completion_tokens: 8,
            total_tokens: 28,
            ..Default::default()
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
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        let parsed: TokenUsage = serde_json::from_str(&s).unwrap();
        assert_eq!(u, parsed);
    }

    // ---- Ch15: persistence ----

    use tempfile::tempdir;

    /// A session carrying every interesting message shape: reasoning, a tool
    /// call, and a tool result.
    fn rich_session() -> Session {
        let mut s = Session::new("be helpful");
        s.push_user("refactor the parser");
        s.push_assistant(
            Message::assistant("on it").with_reasoning("the user wants a refactor".to_owned()),
        );
        s.record_usage(TokenUsage {
            prompt_tokens: 12,
            completion_tokens: 7,
            total_tokens: 19,
            ..Default::default()
        });
        s
    }

    #[test]
    fn session_round_trips_through_json() {
        let s = rich_session();
        let json = serde_json::to_string_pretty(&s).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, parsed);
    }

    #[test]
    fn store_save_then_load_is_identical() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        let s = rich_session();

        store.save("abc", 1000, &s).unwrap();
        assert_eq!(store.load("abc").unwrap(), s);
        assert_eq!(store.created_at("abc").unwrap(), 1000);
    }

    #[test]
    fn save_preserves_caller_supplied_created_at() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        let s = Session::new("sys");

        // Re-saving with the same created_at (as the REPL does each turn) must
        // not bump the timestamp.
        store.save("id1", 5000, &s).unwrap();
        store.save("id1", 5000, &s).unwrap();
        assert_eq!(store.created_at("id1").unwrap(), 5000);
    }

    #[test]
    fn list_is_newest_first_with_title_and_turns() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());

        let mut older = Session::new("sys");
        older.push_user("older question");
        store.save("100", 100, &older).unwrap();

        let mut newer = Session::new("sys");
        newer.push_user("newer question");
        newer.push_assistant(Message::assistant("answer"));
        store.save("200", 200, &newer).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "200"); // newest first
        assert_eq!(list[0].title, "newer question");
        assert_eq!(list[0].turns, 2);
        assert_eq!(list[1].id, "100");
        assert_eq!(list[1].turns, 1);
    }

    #[test]
    fn list_on_missing_dir_is_empty() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().join("nope"));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn load_missing_id_is_an_error() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        assert!(matches!(store.load("ghost"), Err(SessionError::Io(_))));
    }

    #[test]
    fn delete_removes_the_session_file() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        store.save("gone", 1, &Session::new("sys")).unwrap();
        assert!(store.path_for("gone").exists());

        store.delete("gone").unwrap();

        assert!(!store.path_for("gone").exists());
        assert!(matches!(store.load("gone"), Err(SessionError::Io(_))));
    }

    #[test]
    fn delete_missing_id_is_an_error() {
        let dir = tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        assert!(matches!(store.delete("ghost"), Err(SessionError::Io(_))));
    }

    #[test]
    fn new_id_is_nonempty_numeric() {
        let id = SessionStore::new_id();
        assert!(!id.is_empty());
        assert!(id.parse::<u64>().is_ok());
    }

    #[test]
    fn title_truncates_long_first_user_message() {
        let mut s = Session::new("sys");
        s.push_user("x".repeat(200));
        assert!(summarize_title(&s).ends_with('…'));
        assert!(summarize_title(&s).chars().count() <= 61);
    }
}
