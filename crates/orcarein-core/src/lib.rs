//! Core library for OrcaRein agent harness.
//!
//! This crate exposes the `Provider` and `Tool` traits and core types
//! shared between the binary and external embedders.

pub mod config;
pub mod message;
pub mod permission;
pub mod provider;
pub mod session;
pub mod tool;

pub use config::{env_key_var, Config, ConfigError, SecretStore, CONFIG_KEYS};
pub use message::Message;
pub use permission::{Decision, PermissionStore};
pub use provider::{
    ChatOptions, DeepSeekProvider, MockProvider, OpenAIProvider, Provider, StreamEvent,
};
pub use session::{Session, SessionError, SessionStore, SessionSummary, TokenUsage};
pub use tool::{
    // Ch11 — full tool suite
    BashTool,
    EditTool,
    // Ch09 protocol types
    FunctionCall,
    FunctionDefinition,
    ListDirTool,
    // Ch10 abstraction layer + first tool
    ReadFileTool,
    RiskLevel,
    Tool,
    ToolCall,
    ToolDefinition,
    ToolError,
    ToolOutput,
    ToolRegistry,
    ToolSchema,
    WriteFileTool,
};

/// Returns the crate version (semver string).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        assert!(!version().is_empty());
    }
}
