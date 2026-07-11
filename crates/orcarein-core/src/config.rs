//! Layered configuration: a TOML `Config` plus a `SecretStore` for API keys.
//!
//! `orcarein-core` owns only the **pure** persistence pieces — reading and
//! writing `config.toml` / `secrets.toml`, resolving the per-platform config
//! directory, and string-keyed get/set for the `config` subcommand. The
//! binary layers precedence on top (CLI flag > env var > TOML > default).
//!
//! Two files live side by side under the platform config dir
//! (`~/.config/orcarein/` on Linux, `%APPDATA%\orcarein\` on Windows):
//!
//! - `config.toml`  — non-secret preferences ([`Config`]).
//! - `secrets.toml` — API keys ([`SecretStore`]), `0600` on Unix.
//!
//! API keys are deliberately kept out of [`Config`] and have no CLI flag, so
//! they never land in shell history or a process list. They resolve from the
//! environment first, then `secrets.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hook::HooksConfig;
use crate::permission::PermissionRule;

/// Errors from loading or saving configuration / secrets.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed TOML: {0}")]
    ParseToml(#[from] toml::de::Error),

    #[error("could not serialize TOML: {0}")]
    SerializeToml(#[from] toml::ser::Error),

    #[error("unknown config key '{0}' (known: provider, model, tools, system_prompt)")]
    UnknownKey(String),

    #[error("could not locate a config directory for this platform")]
    NoConfigDir,
}

/// The set of keys the `config get/set/list` subcommand understands.
pub const CONFIG_KEYS: &[&str] = &["provider", "model", "tools", "system_prompt"];

/// Non-secret user preferences, round-tripped to `config.toml`.
///
/// Every field is optional: a missing file, or a file that sets only some
/// keys, both deserialize cleanly to [`Default`]. The binary treats `None`
/// as "fall through to the next precedence layer".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Provider id, e.g. `deepseek` or `openai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id; `None` means "use the provider's default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whitelist of tool names the REPL may expose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_allowlist: Option<Vec<String>>,
    /// Override for the system prompt that steers the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// MCP servers to launch and expose tools from. Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServerConfig>,
    /// HTTP retry policy knob (only max_retries is user-tunable). None = defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    /// Declarative permission rules (allow/ask/deny). None = only built-in
    /// sensitive-path defaults apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionConfig>,
    /// User-configured tool hooks (PreToolUse / PostToolUse). None = no hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksConfig>,
}

/// One MCP server the client should launch and expose tools from.
///
/// Defined here (not under the `mcp` feature) so `Config` compiles
/// regardless of the feature; `crate::mcp` re-exports it. Derives must match
/// `Config`'s (Debug/Clone/PartialEq/Eq/Serialize/Deserialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Logical name; tools surface as `mcp__<name>__<tool>`.
    pub name: String,
    /// Executable to launch (e.g. `npx`).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// The `[retry]` section: the single user-tunable retry knob.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Max retries for transient provider HTTP failures. None → default (3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
}

/// The `[permissions]` section: a list of allow/ask/deny rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PermissionRule>,
}

/// The qualifier/org/app triple identifying OrcaRein's config directory.
fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "orcarein")
}

impl Config {
    /// The default config path (`<config_dir>/config.toml`), if the platform
    /// exposes a config directory.
    pub fn config_path() -> Option<PathBuf> {
        project_dirs().map(|d| d.config_dir().join("config.toml"))
    }

    /// Loads from the default path. A missing file is **not** an error — it
    /// yields [`Config::default`].
    pub fn load() -> Result<Self, ConfigError> {
        let path = Config::config_path().ok_or(ConfigError::NoConfigDir)?;
        Config::load_from(&path)
    }

    /// Loads from an explicit path. A missing file yields [`Config::default`].
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Saves to the default path, creating parent directories as needed.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Config::config_path().ok_or(ConfigError::NoConfigDir)?;
        self.save_to(&path)
    }

    /// Saves to an explicit path, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Returns the current value of `key` as a display string, or `None` if
    /// the key is unset. `tools` is rendered comma-joined.
    pub fn get(&self, key: &str) -> Result<Option<String>, ConfigError> {
        match key {
            "provider" => Ok(self.provider.clone()),
            "model" => Ok(self.model.clone()),
            "system_prompt" => Ok(self.system_prompt.clone()),
            "tools" => Ok(self.tools_allowlist.as_ref().map(|v| v.join(","))),
            other => Err(ConfigError::UnknownKey(other.to_owned())),
        }
    }

    /// Sets `key` to `value`. `tools` is parsed as a comma-separated list
    /// (blank entries dropped). Unknown keys are rejected.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        match key {
            "provider" => self.provider = Some(value.to_owned()),
            "model" => self.model = Some(value.to_owned()),
            "system_prompt" => self.system_prompt = Some(value.to_owned()),
            "tools" => {
                let list: Vec<String> = value
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.tools_allowlist = Some(list);
            }
            other => return Err(ConfigError::UnknownKey(other.to_owned())),
        }
        Ok(())
    }

    /// All known keys with their current values, for `config list`.
    pub fn entries(&self) -> Vec<(&'static str, Option<String>)> {
        CONFIG_KEYS
            .iter()
            .map(|&k| (k, self.get(k).expect("CONFIG_KEYS entries are all known")))
            .collect()
    }
}

/// The environment variable holding the API key for a given provider.
pub fn env_key_var(provider: &str) -> Option<&'static str> {
    match provider {
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    }
}

/// API keys persisted to `secrets.toml`, keyed by provider id.
///
/// Kept separate from [`Config`] so the secret file can be locked down
/// (`0600` on Unix) and so keys never appear in a non-secret dump. A future
/// chapter can add an OS-keychain backend behind this same type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretStore {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

impl SecretStore {
    /// The default secrets path (`<config_dir>/secrets.toml`).
    pub fn secrets_path() -> Option<PathBuf> {
        project_dirs().map(|d| d.config_dir().join("secrets.toml"))
    }

    /// Loads from the default path. A missing file yields an empty store.
    pub fn load() -> Result<Self, ConfigError> {
        let path = SecretStore::secrets_path().ok_or(ConfigError::NoConfigDir)?;
        SecretStore::load_from(&path)
    }

    /// Loads from an explicit path. A missing file yields an empty store.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SecretStore::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// The stored key for `provider`, if any (file only — see [`resolve`]).
    ///
    /// [`resolve`]: SecretStore::resolve
    pub fn get(&self, provider: &str) -> Option<&str> {
        self.keys.get(provider).map(String::as_str)
    }

    /// Stores `key` for `provider`, overwriting any previous value.
    pub fn set(&mut self, provider: &str, key: &str) {
        self.keys.insert(provider.to_owned(), key.to_owned());
    }

    /// Resolves the effective API key for `provider`: the environment
    /// variable wins, then the stored secret. `None` if neither is set.
    pub fn resolve(&self, provider: &str) -> Option<String> {
        if let Some(var) = env_key_var(provider) {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
        self.get(provider).map(str::to_owned)
    }

    /// Saves to the default path.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = SecretStore::secrets_path().ok_or(ConfigError::NoConfigDir)?;
        self.save_to(&path)
    }

    /// Saves to an explicit path, creating parents and locking the file to
    /// `0600` on Unix. On other platforms (Windows) we cannot set Unix mode
    /// bits, so we warn that the file relies on the profile directory's ACL.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, &text)?;
        Self::restrict_permissions(path)?;
        Ok(())
    }

    #[cfg(unix)]
    fn restrict_permissions(path: &Path) -> Result<(), ConfigError> {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn restrict_permissions(path: &Path) -> Result<(), ConfigError> {
        eprintln!(
            "warning: wrote {} without restrictive permissions \
             (relying on your user profile directory's ACL)",
            path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_is_empty() {
        let c = Config::default();
        assert!(c.provider.is_none());
        assert!(c.model.is_none());
        assert!(c.tools_allowlist.is_none());
        assert!(c.system_prompt.is_none());
    }

    #[test]
    fn load_missing_file_is_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
    }

    #[test]
    fn toml_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut c = Config::default();
        c.set("provider", "openai").unwrap();
        c.set("model", "gpt-4o").unwrap();
        c.set("tools", "read_file, list_dir").unwrap();
        c.set("system_prompt", "be terse").unwrap();

        c.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(
            loaded.tools_allowlist.as_deref(),
            Some(&["read_file".to_owned(), "list_dir".to_owned()][..])
        );
    }

    #[test]
    fn partial_toml_deserializes() {
        let c: Config = toml::from_str("provider = \"deepseek\"\n").unwrap();
        assert_eq!(c.provider.as_deref(), Some("deepseek"));
        assert!(c.model.is_none());
        assert!(c.tools_allowlist.is_none());
    }

    #[test]
    fn get_set_known_keys() {
        let mut c = Config::default();
        assert_eq!(c.get("provider").unwrap(), None);
        c.set("provider", "openai").unwrap();
        assert_eq!(c.get("provider").unwrap().as_deref(), Some("openai"));
        c.set("tools", "bash,edit").unwrap();
        assert_eq!(c.get("tools").unwrap().as_deref(), Some("bash,edit"));
    }

    #[test]
    fn set_unknown_key_errors() {
        let mut c = Config::default();
        let err = c.set("nope", "x").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownKey(k) if k == "nope"));
        assert!(matches!(c.get("nope"), Err(ConfigError::UnknownKey(_))));
    }

    #[test]
    fn entries_lists_all_keys() {
        let c = Config::default();
        let keys: Vec<&str> = c.entries().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, CONFIG_KEYS);
    }

    #[test]
    fn env_key_var_maps_known_providers() {
        assert_eq!(env_key_var("deepseek"), Some("DEEPSEEK_API_KEY"));
        assert_eq!(env_key_var("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(env_key_var("mystery"), None);
    }

    #[test]
    fn secrets_roundtrip_and_get() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.toml");

        let mut s = SecretStore::default();
        assert!(s.get("deepseek").is_none());
        s.set("deepseek", "sk-fake-123");
        s.save_to(&path).unwrap();

        let loaded = SecretStore::load_from(&path).unwrap();
        assert_eq!(loaded.get("deepseek"), Some("sk-fake-123"));
        assert!(loaded.get("openai").is_none());
    }

    #[test]
    fn secrets_load_missing_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("none.toml");
        let s = SecretStore::load_from(&path).unwrap();
        assert_eq!(s, SecretStore::default());
    }

    #[cfg(unix)]
    #[test]
    fn secrets_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        let mut s = SecretStore::default();
        s.set("openai", "sk-fake");
        s.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn config_roundtrips_retry_section() {
        let toml_src = "[retry]\nmax_retries = 5\n";
        let cfg: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.retry.as_ref().and_then(|r| r.max_retries), Some(5));

        // Default has no [retry] and round-trips clean (skip_serializing_if).
        let empty = Config::default();
        assert!(empty.retry.is_none());
        let s = toml::to_string(&empty).unwrap();
        assert!(!s.contains("retry"));
    }

    #[test]
    fn config_roundtrips_mcp_servers() {
        let toml_src = r#"
[[mcp_servers]]
name = "fs"
command = "npx"
args = ["-y", "server-filesystem", "/tmp"]
[mcp_servers.env]
TOKEN = "x"
"#;
        let cfg: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers[0].name, "fs");
        assert_eq!(cfg.mcp_servers[0].command, "npx");
        assert_eq!(
            cfg.mcp_servers[0].args,
            vec!["-y", "server-filesystem", "/tmp"]
        );
        assert_eq!(
            cfg.mcp_servers[0].env.get("TOKEN").map(String::as_str),
            Some("x")
        );

        // Default has no servers and round-trips clean (skip_serializing_if).
        let empty = Config::default();
        assert!(empty.mcp_servers.is_empty());
        let s = toml::to_string(&empty).unwrap();
        assert!(!s.contains("mcp_servers"));
    }

    #[test]
    fn config_roundtrips_permission_rules() {
        let toml_src =
            "[[permissions.rules]]\ntool = \"bash\"\ncommand = \"git *\"\naction = \"allow\"\n";
        let cfg: Config = toml::from_str(toml_src).unwrap();
        let rules = &cfg.permissions.as_ref().unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tool, "bash");
        assert_eq!(rules[0].command.as_deref(), Some("git *"));

        // Default omits [permissions].
        let empty = Config::default();
        assert!(empty.permissions.is_none());
        assert!(!toml::to_string(&empty).unwrap().contains("permissions"));
    }

    #[test]
    fn config_roundtrips_hooks() {
        let toml_src = "[[hooks.PreToolUse]]\nmatcher = \"bash\"\ncommand = \"guard.sh\"\n";
        let cfg: Config = toml::from_str(toml_src).unwrap();
        let h = cfg.hooks.as_ref().unwrap();
        assert_eq!(h.pre_tool_use.len(), 1);
        assert_eq!(h.pre_tool_use[0].matcher, "bash");
        assert_eq!(h.pre_tool_use[0].command, "guard.sh");
        assert!(h.post_tool_use.is_empty());

        // Default omits [hooks].
        let empty = Config::default();
        assert!(empty.hooks.is_none());
        assert!(!toml::to_string(&empty).unwrap().contains("hooks"));
    }
}
