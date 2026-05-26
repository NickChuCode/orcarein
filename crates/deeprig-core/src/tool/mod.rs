//! Tool abstraction — `trait Tool`, the registry, and the supporting
//! types (`ToolOutput`, `RiskLevel`, `ToolError`).
//!
//! Chapter 9 added the wire-format types in `protocol` (so the harness
//! could parse `tool_calls` it would receive). Chapter 10 turns those
//! types into something callable: a registry of `Box<dyn Tool>` plus the
//! first concrete tool, `read_file`. The REPL builds a registry, sends
//! its `definitions()` along with every request, and dispatches the
//! returned `tool_calls` back into the registry — that loop lives in the
//! binary (`crates/deeprig/src/main.rs`).
//!
//! Module layout:
//! - `protocol` — DeepSeek/OpenAI wire-format types (Ch09, moved verbatim).
//! - `read_file` — the first concrete `Tool` impl (Ch10).
//! - this file — `trait Tool`, `ToolRegistry`, `ToolOutput`, `RiskLevel`,
//!   `ToolError`.

pub mod bash;
pub mod edit;
pub mod list_dir;
pub mod protocol;
pub mod read_file;
pub mod write_file;

pub use bash::BashTool;
pub use edit::EditTool;
pub use list_dir::ListDirTool;
pub use protocol::{FunctionCall, FunctionDefinition, ToolCall, ToolDefinition, ToolSchema};
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

use async_trait::async_trait;
use std::collections::BTreeMap;

/// What a tool produces when it runs.
///
/// Currently just a string of content. Future chapters may add metadata
/// (truncation flags, MIME type, structured payloads); keeping it as a
/// struct (rather than a bare `String`) makes that growth non-breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// Text payload returned to the model as the `content` of a `role=tool`
    /// message.
    pub content: String,
}

impl ToolOutput {
    /// Wraps a string in a `ToolOutput`.
    pub fn new(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
        }
    }
}

impl From<String> for ToolOutput {
    fn from(s: String) -> Self {
        ToolOutput { content: s }
    }
}

/// How risky a tool is — used by Ch12's permission prompt to decide
/// whether to ask the user before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Read-only / pure-information tools (e.g. `read_file`, `list_dir`).
    Safe,
    /// Tools that can modify the filesystem or environment (e.g.
    /// `write_file`, `bash`).
    Risky,
}

/// Errors a tool can produce.
///
/// `InvalidArguments` covers schema-mismatch (the model handed us bogus
/// JSON for our parameters); `Io` covers filesystem / OS errors during
/// execution; `Other` is the escape hatch for tool-specific failures that
/// do not fit either bucket.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid arguments: {0}")]
    InvalidArguments(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// A tool the model can call.
///
/// `name`, `description`, and `schema` produce the `ToolDefinition` that
/// gets sent to the model so it knows what is available. `execute`
/// receives the parsed JSON arguments and returns the tool's output.
///
/// Implementations live in `tool/<name>.rs` (one file per tool).
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique identifier — appears as `function.name` on the wire. Must
    /// be stable across versions (the model relies on the name).
    fn name(&self) -> &str;

    /// One-line human-readable description shown to the model so it can
    /// decide when to call this tool.
    fn description(&self) -> &str;

    /// JSON Schema describing this tool's `arguments` object.
    fn schema(&self) -> serde_json::Value;

    /// How risky this tool is — Ch12's permission prompt reads this.
    fn risk_level(&self) -> RiskLevel;

    /// Execute the tool with the parsed JSON arguments and return its
    /// output. The dispatcher feeds the result back to the model as a
    /// `role=tool` message.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError>;
}

/// Owns the set of tools the REPL exposes to the model.
///
/// `BTreeMap` (not `HashMap`) so `definitions()` and `names()` are
/// deterministic — stable order across runs is friendlier to caching and
/// to test assertions.
pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        ToolRegistry {
            tools: BTreeMap::new(),
        }
    }

    /// Adds a tool. Panics if a tool with the same name is already
    /// registered — duplicates are a programmer error in v0.1.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_owned();
        assert!(
            !self.tools.contains_key(&name),
            "tool '{name}' is already registered"
        );
        self.tools.insert(name, tool);
    }

    /// Looks up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|boxed| boxed.as_ref())
    }

    /// `ToolDefinition`s ready to drop into the request body, in
    /// deterministic order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition::function(t.name(), t.description(), t.schema()))
            .collect()
    }

    /// Registered tool names in deterministic order.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    /// `true` if no tools have been registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        ToolRegistry::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_new_is_empty() {
        let r = ToolRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.names().is_empty());
        assert!(r.definitions().is_empty());
    }

    #[test]
    fn registry_default_matches_new() {
        let a = ToolRegistry::default();
        let b = ToolRegistry::new();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.names(), b.names());
    }

    #[test]
    fn registry_register_and_get() {
        let mut r = ToolRegistry::new();
        r.register(Box::new(ReadFileTool));
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());

        let tool = r.get("read_file").expect("read_file should be registered");
        assert_eq!(tool.name(), "read_file");

        assert!(r.get("nope").is_none());
    }

    #[test]
    fn registry_definitions_lists_each_tool() {
        let mut r = ToolRegistry::new();
        r.register(Box::new(ReadFileTool));
        let defs = r.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].kind, "function");
        assert_eq!(defs[0].function.name, "read_file");
        // Schema is forwarded from the tool.
        assert_eq!(defs[0].function.parameters["type"], "object");
    }

    #[test]
    fn registry_names_is_sorted() {
        // BTreeMap means names() comes back alphabetically — verify with
        // two synthetic tools so the assertion does not depend on having
        // multiple real tools registered.
        struct DummyTool {
            name: &'static str,
        }
        #[async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str {
                self.name
            }
            fn description(&self) -> &str {
                "test"
            }
            fn schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn risk_level(&self) -> RiskLevel {
                RiskLevel::Safe
            }
            async fn execute(&self, _: serde_json::Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::new(""))
            }
        }

        let mut r = ToolRegistry::new();
        r.register(Box::new(DummyTool { name: "zzz" }));
        r.register(Box::new(DummyTool { name: "aaa" }));
        r.register(Box::new(DummyTool { name: "mmm" }));
        assert_eq!(r.names(), vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn registry_double_register_panics() {
        let mut r = ToolRegistry::new();
        r.register(Box::new(ReadFileTool));
        r.register(Box::new(ReadFileTool));
    }

    #[test]
    fn tool_output_from_string_works() {
        let s = String::from("hello");
        let o: ToolOutput = s.into();
        assert_eq!(o.content, "hello");
    }

    #[test]
    fn tool_output_new_works() {
        let o = ToolOutput::new("hi");
        assert_eq!(o.content, "hi");
    }

    #[test]
    fn tool_error_displays_variant_message() {
        let e = ToolError::Other("boom".into());
        assert_eq!(e.to_string(), "boom");
    }
}
