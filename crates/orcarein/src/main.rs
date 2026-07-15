//! OrcaRein CLI — interactive chat REPL with streaming, session state,
//! tool dispatch, permission gating, and pluggable model providers.
//!
//! Chapter 14 milestone: a layered config system. `clap` (derive) parses the
//! CLI and a `config get/set/list` subcommand; effective settings resolve in
//! the order **CLI flag > env var > `config.toml` > built-in default**. API
//! keys come from the env or `secrets.toml` (never a CLI flag), via
//! `SecretStore`.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use orcarein_core::cost;
use orcarein_core::doctor::{self, Check, CheckStatus};
use orcarein_core::{
    env_key_var, fetch_issue, parse_owner_repo, Agent, AgentEvent, AllowlistPolicy, BashTool,
    CacheMode, Config, Decision, DeepSeekProvider, EditTool, EventSink, HookSet, ListDirTool,
    OpenAIProvider, PermissionConfig, PermissionMode, PermissionPolicy, PermissionRule,
    PermissionStore, Provider, ReadFileTool, RetryPolicy, RiskLevel, RuleAction, Ruleset,
    SearchTool, SecretStore, Session, SessionStore, SessionSummary, SharedMode, SkillTool,
    SubagentTool, Tool, ToolRegistry, WriteFileTool, DEFAULT_SUBAGENT_PERSONA, MAX_TOOL_ITERATIONS,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod color;
mod header;
#[cfg(feature = "hardware")]
mod hwmon;
#[cfg(feature = "tui")]
mod markdown;
#[cfg(feature = "tui")]
mod modal;
mod overlay;
#[cfg(feature = "tui")]
mod syntax;
// A terminal-UI ornament. `whale` itself needs no ratatui — it is escape codes,
// not widgets — but the lean `--no-default-features` build reports `fancy=false`
// (see `header_env`), so a whale could never surface there. Gated to keep the
// lean binary honest rather than carrying code that can't run.
#[cfg(feature = "tui")]
mod whale;

/// Demo pins watched by `hw monitor` when `--pins` is omitted.
#[cfg(feature = "hardware")]
const DEFAULT_MONITOR_PINS: &[u8] = &[17, 27, 22, 23, 24, 25];

/// Fallback system prompt when neither `--system-prompt-file` nor the config
/// file supplies one.
const DEFAULT_SYSTEM_PROMPT: &str = "You are OrcaRein, a concise and helpful CLI assistant.";

/// Names of every built-in tool — keeps `--tools` typo warnings honest.
const KNOWN_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "list_dir",
    "bash",
    "edit",
    "search",
];

/// Outcome of handling a slash command — whether the loop continues or quits.
enum CommandAction {
    Continue,
    Quit,
    RunInit,
    RunCompact,
    /// `/model <name>` — switch the active model (same provider) at runtime.
    SwitchModel(String),
    /// `/resume <id|prefix>` — switch the live session at runtime.
    SwitchSession(String),
    /// `/new` — start a fresh session (new id/file, empty history) at runtime.
    NewSession,
    /// `/orca` — play the swimming-whale animation once (cosmetic).
    Swim,
}

/// OrcaRein — an open-source CLI agent harness for DeepSeek V4 and
/// OpenAI-compatible models.
#[derive(Parser, Debug)]
#[command(name = "orcarein", version, about, long_about = None)]
struct Cli {
    /// Provider-specific model id (overrides config and the provider default).
    #[arg(value_name = "MODEL")]
    model: Option<String>,

    /// Backend provider: `deepseek` (default) or `openai`.
    #[arg(long)]
    provider: Option<String>,

    /// Skip permission prompts. Requires non-tty stdin (a safety guard).
    #[arg(long)]
    no_permission: bool,

    /// Session permission mode: default, acceptEdits, plan, yolo.
    #[arg(long, global = true, value_parser = parse_permission_mode)]
    permission_mode: Option<PermissionMode>,

    /// Comma-separated tool whitelist (e.g. `read_file,list_dir`).
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Benchmark mode: defeat DeepSeek's prefix cache (perturb each request)
    /// so you can A/B the savings the stable prefix earns. Demo only.
    #[arg(long)]
    no_economy: bool,

    /// Read the system prompt from a file instead of the built-in default.
    #[arg(long, value_name = "PATH")]
    system_prompt_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// clap `value_parser` for `--permission-mode`. `core` can't derive
/// `clap::ValueEnum` on `PermissionMode` (it doesn't depend on clap), so the
/// bin side parses through the `FromStr` impl T1 already provides.
fn parse_permission_mode(s: &str) -> Result<PermissionMode, String> {
    s.parse()
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Get, set, or list persisted configuration in `config.toml`.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List saved sessions or resume one by id.
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Run offline health checks and print a PASS/WARN/FAIL report.
    Doctor,
    /// Run a single task non-interactively and exit (headless / scriptable).
    ///
    /// Reads the prompt from the argument, or from stdin if omitted. Writes the
    /// final answer to stdout and diagnostics to stderr. Risky tools are
    /// **denied** unless named with `--allow` (or `--no-permission`).
    Run {
        /// The task/prompt. If omitted, OrcaRein reads it from stdin.
        prompt: Option<String>,
        /// Allow these Risky tools without prompting (comma-separated, e.g.
        /// `bash,edit`). Without it, `run` denies every Risky tool.
        #[arg(long, value_delimiter = ',')]
        allow: Option<Vec<String>>,
    },
    /// Fix a GitHub issue in the current repo (BYO-key self-bootstrap loop).
    ///
    /// Reads issue #N from the `origin` remote's repo, lets the agent edit the
    /// code (read/list/edit/write only — no shell), runs `cargo test`, and
    /// shows the diff for you to review. It never commits, pushes, or opens a
    /// PR — that's your call. Requires a clean working tree.
    Issue {
        /// The issue number to fix.
        number: u64,
    },
    /// GPIO live monitor (experimental hardware wedge — build `--features hardware`).
    #[cfg(feature = "hardware")]
    Hw {
        #[command(subcommand)]
        action: HwAction,
    },
    /// Store an API key once in secrets.toml (no per-session env var needed).
    ///
    /// Prompts for the key without echoing it, verifies it against the
    /// provider (unless `--no-verify`), and writes it 0600. Never accepts the
    /// key as a flag (that would leak it in shell history / the process list).
    Login {
        /// Provider to store the key for (default: your configured provider,
        /// else `deepseek`).
        #[arg(long)]
        provider: Option<String>,
        /// Skip the `/v1/models` verification call and just save the key.
        #[arg(long)]
        no_verify: bool,
    },
}

/// Actions for the `hw` subcommand.
#[cfg(feature = "hardware")]
#[derive(Subcommand, Debug, Clone)]
enum HwAction {
    /// Live-monitor GPIO pin levels (demo data until the real backend lands at M2).
    Monitor {
        /// Pins to watch (comma-separated, e.g. `17,27,22`). Defaults to a demo set.
        #[arg(long, value_delimiter = ',')]
        pins: Vec<u8>,
        /// Refresh interval in milliseconds.
        #[arg(long, default_value_t = 500)]
        interval: u64,
        /// Print a single snapshot and exit (no live overlay).
        #[arg(long)]
        once: bool,
    },
}

/// Actions for the `config` subcommand. Mirrors the `Config::get/set/entries`
/// surface in `orcarein-core`.
#[derive(Subcommand, Debug, Clone)]
enum ConfigAction {
    /// Print the value of one key (`provider`, `model`, `tools`, `system_prompt`).
    Get { key: String },
    /// Set a key and persist it to `config.toml`.
    Set { key: String, value: String },
    /// List every key and its current value.
    List,
}

/// Actions for the `session` subcommand.
#[derive(Subcommand, Debug, Clone)]
enum SessionAction {
    /// List saved sessions, newest first.
    List,
    /// Resume a saved session (by full id or an unambiguous prefix), then
    /// continue chatting. With no id, shows a numbered menu of saved sessions
    /// to pick from (interactive terminals only).
    Resume { id: Option<String> },
    /// Delete a saved session by id or an unambiguous prefix (auto-save never
    /// prunes — this is how you clean up). Reports cleanly if nothing matches,
    /// and lists candidates if a prefix is ambiguous.
    Delete { id: String },
}

/// Effective settings after resolving CLI > env > config > defaults.
struct Resolved {
    provider: Arc<dyn Provider>,
    model: String,
    system_prompt: String,
    tools_allowlist: Option<Vec<String>>,
    /// User-authored permission rules from `config.toml`'s `[[permissions.rules]]`,
    /// merged into the Agent's [`Ruleset`] on top of the built-in sensitive-path
    /// defaults. Empty when the user has no `[permissions]` section.
    permission_rules: Vec<PermissionRule>,
    /// User-authored PreToolUse/PostToolUse hooks from `config.toml`'s
    /// `[[hooks.PreToolUse]]`/`[[hooks.PostToolUse]]`. Empty when the user has
    /// no `[hooks]` section.
    hooks: HookSet,
    /// Effective session permission mode: `--permission-mode` >
    /// `--no-permission` (yolo) > `[permissions] mode` > default.
    // Not read via `run`/`issue` yet (their `Resolved { .. }` destructures
    // don't name it) — a follow-up cut wires it into a runtime `SharedMode`.
    #[allow(dead_code)]
    perm_mode: PermissionMode,
}

/// Everything the CLI/env/config chain yields that does **not** need an API key.
///
/// This split is the whole point of the cut: `resolve()` used to build the
/// `Provider` in the middle of settings resolution, so a missing key killed the
/// entire startup prologue — including the welcome box we now want to draw
/// *before* asking for a key.
struct Settings {
    provider: String,
    model: String,
    system_prompt: String,
    tools_allowlist: Option<Vec<String>>,
    permission_rules: Vec<PermissionRule>,
    hooks: HookSet,
    retry: RetryPolicy,
    perm_mode: PermissionMode,
}

/// The single source of truth for "is this a provider we support".
///
/// This message used to exist in more than one place (`build_provider`, and
/// `run_login` via `env_key_var`'s contract). Adding a provider must touch one
/// arm, not hunt for copies.
fn validate_provider(name: &str) -> Result<()> {
    match name {
        "deepseek" | "openai" => Ok(()),
        other => bail!("unknown provider: '{other}' (expected: deepseek | openai)"),
    }
}

/// What to do at startup about the API key.
#[derive(Debug, PartialEq, Eq)]
enum FirstRun {
    /// A key is already available (env or secrets.toml).
    Proceed,
    /// No key, but a human is sitting there — run the inline login.
    Prompt,
    /// No key and nobody to ask — fail the way we always have.
    Bail,
}

/// Both ends must be a terminal before we may prompt for a secret.
///
/// `&&`, emphatically not `||`: with stdout a tty and stdin a file
/// (`orcarein < prompts.txt`), prompting would read the first line of the user's
/// script as their API key — and an offline verify would then save it.
fn interactive(stdin_tty: bool, stdout_tty: bool) -> bool {
    stdin_tty && stdout_tty
}

/// Blank counts as absent, mirroring `SecretStore::resolve`'s trim.
fn first_run_decision(key: Option<&str>, interactive: bool) -> FirstRun {
    match key {
        Some(k) if !k.trim().is_empty() => FirstRun::Proceed,
        _ if interactive => FirstRun::Prompt,
        _ => FirstRun::Bail,
    }
}

/// Where a first-time user goes to get a key.
fn signup_url(provider: &str) -> Option<&'static str> {
    match provider {
        "deepseek" => Some("https://platform.deepseek.com/api_keys"),
        "openai" => Some("https://platform.openai.com/api-keys"),
        _ => None,
    }
}

/// Builds a `Provider` from a resolved name + optional API key. Validates the
/// provider name first so a missing key never masks a typo'd provider.
fn build_provider(
    name: &str,
    api_key: Option<String>,
    retry: RetryPolicy,
) -> Result<Arc<dyn Provider>> {
    validate_provider(name)?;
    let var = env_key_var(name).expect("provider validated above");
    let key = api_key.with_context(|| {
        format!(
            "no API key for provider '{name}'. Run 'orcarein login' to store one, \
             or set $env:{var} = '<your-key>'"
        )
    })?;
    match name {
        "deepseek" => Ok(Arc::new(DeepSeekProvider::new(key).with_retry(retry))),
        "openai" => Ok(Arc::new(OpenAIProvider::new(key).with_retry(retry))),
        _ => unreachable!("provider validated above"),
    }
}

/// Treats a blank string as absent, so `--provider ""` or `ORCAREIN_PROVIDER=`
/// (set-but-empty) fall through to the next precedence layer rather than
/// becoming a literal empty value.
fn non_blank(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

/// The result of an optional pre-save key verification (`/v1/models` probe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyOutcome {
    /// The key worked (models listed).
    Verified,
    /// The provider positively rejected the key (401/403) — do not save.
    Rejected,
    /// Couldn't tell (network/5xx/offline) — save with a warning.
    Inconclusive,
}

/// Classify a verify attempt. `None` = the request succeeded (`Ok`). `Some(e)`
/// is the error's `Display`. Only the **status line** (before the `":\n"` body
/// separator) is inspected, so a non-auth status whose body happens to contain
/// "401"/"403" is never a false rejection.
fn classify_verify(err_display: Option<&str>) -> VerifyOutcome {
    let Some(s) = err_display else {
        return VerifyOutcome::Verified;
    };
    let status_line = s.split(":\n").next().unwrap_or(s);
    if status_line.contains(" 401") || status_line.contains(" 403") {
        VerifyOutcome::Rejected
    } else {
        VerifyOutcome::Inconclusive
    }
}

/// Whether an env var value (the provider's `*_API_KEY`) is set and non-blank,
/// so it would override the stored secret in `SecretStore::resolve` (env-first).
/// Pure: the caller does the `std::env::var` read and passes the value in
/// (keeps this deterministic — no process-global env in tests).
fn env_overrides_stored(var_value: Option<&str>) -> bool {
    matches!(var_value, Some(v) if !v.trim().is_empty())
}

/// Reads a line without echoing it, when possible.
///
/// - piped stdin (non-tty): reads a plain line (for `echo $KEY | orcarein login`).
/// - tty + `tui`: raw-mode masked read — one `*` per character (see
///   [`read_secret_masked`] for why stars and not nothing).
/// - tty + `--no-default-features` (no crossterm): plain `read_line` — the key
///   IS echoed (accepted degradation; documented in the QA checklist).
#[cfg(feature = "tui")]
fn read_secret_line(prompt: &str) -> std::io::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        read_secret_masked(prompt)
    } else {
        read_line_plain()
    }
}

#[cfg(not(feature = "tui"))]
fn read_secret_line(prompt: &str) -> std::io::Result<String> {
    use std::io::{IsTerminal, Write};
    if std::io::stdin().is_terminal() {
        // No crossterm without `tui`: cannot mask — the key will be visible.
        print!("{prompt}");
        std::io::stdout().flush()?;
    }
    read_line_plain()
}

/// Plain line read from stdin (used for piped input, and as the no-tui fallback).
fn read_line_plain() -> std::io::Result<String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line)
}

/// Restores cooked terminal mode on drop — covers the `?`-early-return and
/// panic paths (Rust has no `finally`). Mirrors overlay.rs raw-mode handling.
#[cfg(feature = "tui")]
struct RawGuard;
#[cfg(feature = "tui")]
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
}

/// What one key press means while reading a secret. Split out from the event
/// loop so the ugly, platform-specific parts are unit-testable — the Windows
/// Release filter and the Ctrl+C arm order are exactly the two things that
/// silently corrupt a pasted key, and neither was covered before.
#[cfg(feature = "tui")]
#[derive(Debug, PartialEq, Eq)]
enum SecretAction {
    Push(char),
    Pop,
    Submit,
    Cancel,
    Ignore,
}

#[cfg(feature = "tui")]
fn secret_key_action(
    kind: ratatui::crossterm::event::KeyEventKind,
    code: ratatui::crossterm::event::KeyCode,
    mods: ratatui::crossterm::event::KeyModifiers,
) -> SecretAction {
    use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    // Windows delivers a Press AND a Release per keystroke; without this every
    // char doubles. Must come first — before any code match.
    if kind == KeyEventKind::Release {
        return SecretAction::Ignore;
    }
    match code {
        KeyCode::Enter => SecretAction::Submit,
        KeyCode::Esc => SecretAction::Cancel,
        // MUST precede the Char(c) arm below, or Ctrl+C reads as the letter 'c'.
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => SecretAction::Cancel,
        KeyCode::Backspace => SecretAction::Pop,
        // Other control chords (Ctrl+U "kill line", Ctrl+D, Ctrl+W, …) are reflexes
        // people bring from their shell. Without this they'd land in the Char(c) arm
        // below and quietly push a letter into the key — the exact "your key is bad,
        // no it isn't" failure this whole cut exists to eliminate.
        //
        // The ALT exclusion is load-bearing: on Windows AltGr reports as CTRL+ALT, so
        // a German/French keyboard types '@' and '\' with both modifiers set. Dropping
        // every CONTROL'd char would make those keyboards unable to enter an API key.
        KeyCode::Char(_)
            if mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) =>
        {
            SecretAction::Ignore
        }
        KeyCode::Char(c) => SecretAction::Push(c),
        _ => SecretAction::Ignore,
    }
}

/// Masked read: raw mode, one `*` per character, Enter submits, Backspace
/// deletes, Esc/Ctrl-C cancels (returns empty → caller treats as no key).
///
/// The stars are deliberate, and they reverse v02-37's "show nothing, like
/// `sudo`" choice. That threat model was wrong: a `sudo` password is *typed*
/// (so hiding its length defeats shoulder-surfing), but an API key is *pasted*
/// and its length is a public constant. Zero feedback made users doubt the paste
/// landed, paste again, silently concatenate two keys, and get a 401 that told
/// them their key was bad. Leaking a length nobody cares about beats that.
///
/// Raw mode cleared `OPOST`, so a '\n' written in here would NOT become "\r\n"
/// and the display would stair-step. Only `*` and the erase sequence go out
/// before `disable_raw_mode()`.
///
/// A paste arrives here as one `KeyEvent(Char)` per character, because nothing
/// enables bracketed paste. If someone ever turns on `EnableBracketedPaste` for
/// the modal editor, the whole paste becomes a single `Event::Paste`, this
/// `Event::Key` arm stops matching it, and pasted keys vanish silently. Handle
/// `Event::Paste` here if that day comes.
#[cfg(feature = "tui")]
fn read_secret_masked(prompt: &str) -> std::io::Result<String> {
    use ratatui::crossterm::event::{self, Event};
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::Write;

    print!("{prompt}");
    std::io::stdout().flush()?;

    let mut buf = String::new();
    {
        enable_raw_mode()?;
        let _guard = RawGuard; // restores cooked mode on any exit from this block
        loop {
            if let Event::Key(k) = event::read()? {
                match secret_key_action(k.kind, k.code, k.modifiers) {
                    SecretAction::Push(c) => {
                        buf.push(c);
                        print!("*");
                        std::io::stdout().flush()?;
                    }
                    SecretAction::Pop => {
                        if buf.pop().is_some() {
                            print!("\x08 \x08"); // back, overwrite with a space, back
                            std::io::stdout().flush()?;
                        }
                    }
                    SecretAction::Submit => break,
                    SecretAction::Cancel => {
                        buf.clear();
                        break;
                    }
                    SecretAction::Ignore => {}
                }
            }
        } // _guard drops here (or on `?` early-return above) → disable_raw_mode
    }
    let _ = disable_raw_mode(); // idempotent belt-and-suspenders
    println!(); // raw mode swallowed the Enter's newline
    Ok(buf)
}

/// Truncate `s` to at most `max_bytes`, on a char boundary (never splits UTF-8).
/// Used to cap @mention file injections so a huge file can't blow up the prompt.
fn cap_chars(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Resolves everything that does not need an API key, from the precedence chain
/// CLI flag > env var > `config.toml` > built-in default.
fn resolve_settings(cli: &Cli) -> Result<Settings> {
    let config = Config::load().context("failed to load config.toml")?;

    let provider = non_blank(cli.provider.clone())
        .or_else(|| non_blank(std::env::var("ORCAREIN_PROVIDER").ok()))
        .or_else(|| non_blank(config.provider.clone()))
        .unwrap_or_else(|| "deepseek".to_owned());
    // Validate the name up front so a missing key never masks a typo'd provider
    // (and so `default_model_for` below is guaranteed to have an arm).
    validate_provider(&provider)?;

    let retry = RetryPolicy::from_config(config.retry.as_ref().and_then(|r| r.max_retries));

    let model = non_blank(cli.model.clone())
        .or_else(|| non_blank(config.model.clone()))
        .unwrap_or_else(|| {
            orcarein_core::default_model_for(&provider)
                .expect("provider validated above")
                .to_owned()
        });

    let tools_allowlist = cli.tools.clone().or_else(|| config.tools_allowlist.clone());

    let system_prompt = match &cli.system_prompt_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read --system-prompt-file {}", path.display()))?,
        None => config
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_owned()),
    };

    let permission_rules = config
        .permissions
        .as_ref()
        .map(|p| p.rules.clone())
        .unwrap_or_default();

    let hooks = config
        .hooks
        .as_ref()
        .map(HookSet::from_config)
        .unwrap_or_default();

    // Precedence: --permission-mode > --no-permission (=yolo, deprecated) >
    // [permissions] mode > default. `--no-permission` stays functional but
    // warns, steering users toward the mode flag it's now a special case of.
    if cli.no_permission {
        eprintln!("orcarein: warning: --no-permission is deprecated; use --permission-mode yolo");
    }
    let perm_mode = cli
        .permission_mode
        .or(if cli.no_permission {
            Some(PermissionMode::Yolo)
        } else {
            None
        })
        .or(config.permissions.as_ref().and_then(|p| p.mode))
        .unwrap_or_default();

    Ok(Settings {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        permission_rules,
        hooks,
        retry,
        perm_mode,
    })
}

/// Settings **plus** a live `Provider` — i.e. the headless path, which must have
/// an API key or fail. `run` / `issue` keep using this; only the REPL splits the
/// two so it can ask for a key after drawing the welcome box.
fn resolve(cli: &Cli) -> Result<Resolved> {
    let s = resolve_settings(cli)?;
    let secrets = SecretStore::load().context("failed to load secrets.toml")?;
    let provider = build_provider(&s.provider, secrets.resolve(&s.provider), s.retry)?;
    Ok(Resolved {
        provider,
        model: s.model,
        system_prompt: s.system_prompt,
        tools_allowlist: s.tools_allowlist,
        permission_rules: s.permission_rules,
        hooks: s.hooks,
        perm_mode: s.perm_mode,
    })
}

/// Store an API key in secrets.toml. See `Command::Login`.
async fn run_login(cli: &Cli, provider_arg: Option<String>, no_verify: bool) -> Result<()> {
    let config = Config::load().context("failed to load config.toml")?;

    // Provider precedence: --provider, then the same order the REPL uses.
    let provider = non_blank(provider_arg)
        .or_else(|| non_blank(cli.provider.clone()))
        .or_else(|| non_blank(std::env::var("ORCAREIN_PROVIDER").ok()))
        .or_else(|| non_blank(config.provider.clone()))
        .unwrap_or_else(|| "deepseek".to_owned());

    validate_provider(&provider)?;

    // Read the key (masked on a tty, plain when piped / no-tui).
    let key = read_secret_line(&format!("Enter API key for {provider}: "))
        .context("failed to read the API key")?
        .trim()
        .to_string();
    if key.is_empty() {
        bail!("no key entered");
    }

    // Optional verification via /v1/models (short retry so offline returns fast).
    if !no_verify {
        let verify_provider = build_provider(
            &provider,
            Some(key.clone()),
            RetryPolicy::from_config(Some(0)),
        )?;
        let outcome = match verify_provider.list_models().await {
            Ok(_) => classify_verify(None),
            Err(e) => classify_verify(Some(&e.to_string())),
        };
        match outcome {
            VerifyOutcome::Verified => println!("✓ key verified"),
            VerifyOutcome::Rejected => {
                bail!("key was rejected by {provider} (check the key) — not saved")
            }
            VerifyOutcome::Inconclusive => {
                eprintln!("could not verify (offline?), saved anyway — run 'orcarein doctor' later")
            }
        }
    }

    save_key(&provider, &key)?;
    Ok(())
}

/// Persist a key to secrets.toml (0600) and tell the user where it went — they
/// need to know which file to edit later. The path never contains the key.
///
/// Shared by `orcarein login` and the first-run flow so the two can't drift.
fn save_key(provider: &str, key: &str) -> Result<()> {
    let path =
        SecretStore::secrets_path().context("no config directory available to store the key")?;
    let mut store = SecretStore::load().context("failed to load secrets.toml")?;
    if store.get(provider).is_some() {
        eprintln!("replacing existing {provider} key");
    }
    store.set(provider, key);
    store.save().context("failed to write secrets.toml")?;
    println!("saved {provider} API key to {}", path.display());

    // env-first precedence: a set env var still wins over what we just saved.
    if let Some(var) = env_key_var(provider) {
        if env_overrides_stored(std::env::var(var).ok().as_deref()) {
            eprintln!("note: ${var} is set and takes precedence over secrets.toml");
        }
    }
    Ok(())
}

/// What the first-run login learned about the model list while verifying the key.
#[derive(Debug)]
enum ModelsHint {
    /// `/v1/models` succeeded — reuse this list, don't hit the network again.
    Fetched(Vec<String>),
    /// The verify call couldn't reach the API. Don't waste another 2s timeout on it.
    Offline,
}

/// The first-run inline login: guide, read the key, verify, save — then hand the
/// key back so startup can continue into the REPL.
///
/// `Ok(None)` means **the user declined** (empty input / Esc / Ctrl-C); the caller
/// prints guidance and exits 0, because choosing not to log in is not an error.
/// A key the server rejects three times is a *failure*, not a decline — that path
/// bails, exactly like `orcarein login` does.
///
/// Returns the model list from the verifying `/v1/models` call when we have one,
/// so startup doesn't immediately fetch it a second time.
///
/// No `--no-verify` here on purpose: the first run is exactly when a bad key is
/// most likely and most confusing. Being offline is already handled (Inconclusive
/// saves the key with a warning).
async fn first_run_login(provider: &str) -> Result<Option<(String, ModelsHint)>> {
    const ATTEMPTS: usize = 3;

    println!();
    println!("首次使用：需要一个 {provider} API key。");
    if let Some(url) = signup_url(provider) {
        println!("获取：{url}");
    }
    println!("直接回车（或 Esc / Ctrl+C）可跳过并退出。");
    println!();

    for attempt in 1..=ATTEMPTS {
        let key = read_secret_line(&format!("Enter API key for {provider}: "))
            .context("failed to read the API key")?
            .trim()
            .to_string();
        if key.is_empty() {
            return Ok(None); // declined
        }

        // Verify with zero retries so an offline machine returns fast instead of
        // sleeping through the backoff ladder.
        let verifier = build_provider(
            provider,
            Some(key.clone()),
            RetryPolicy::from_config(Some(0)),
        )?;
        let (outcome, models) = match verifier.list_models().await {
            Ok(v) => (classify_verify(None), Some(v)),
            Err(e) => (classify_verify(Some(&e.to_string())), None),
        };

        match outcome {
            VerifyOutcome::Verified => {
                println!("✓ key verified");
                save_key(provider, &key)?;
                let hint = match models {
                    Some(v) => ModelsHint::Fetched(v),
                    None => ModelsHint::Offline,
                };
                return Ok(Some((key, hint)));
            }
            VerifyOutcome::Inconclusive => {
                eprintln!(
                    "could not verify (offline?), saved anyway — run 'orcarein doctor' later"
                );
                save_key(provider, &key)?;
                return Ok(Some((key, ModelsHint::Offline)));
            }
            VerifyOutcome::Rejected => {
                // NOT saved (spec P2). Retry in place: a mis-pasted key is the
                // single most likely first-run failure, and making the user quit
                // and re-run `orcarein login` for it is punishment, not guidance.
                let left = ATTEMPTS - attempt;
                if left == 0 {
                    bail!("key was rejected by {provider} (check the key) — not saved");
                }
                eprintln!("key 被 {provider} 拒绝，再试一次（还剩 {left} 次）。");
            }
        }
    }
    unreachable!("the loop either returns or bails on the last attempt")
}

/// Runs an `orcarein config get/set/list` invocation, then returns.
fn run_config(action: ConfigAction) -> Result<()> {
    let mut config = Config::load().context("failed to load config.toml")?;
    match action {
        ConfigAction::Get { key } => match config.get(&key)? {
            Some(v) => println!("{v}"),
            None => println!("(unset)"),
        },
        ConfigAction::Set { key, value } => {
            config.set(&key, &value)?;
            config.save().context("failed to save config.toml")?;
            let where_ = Config::config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(unknown path)".to_owned());
            println!("set {key} = {value}  →  {where_}");
        }
        ConfigAction::List => {
            for (k, v) in config.entries() {
                println!("{k} = {}", v.as_deref().unwrap_or("(unset)"));
            }
        }
    }
    Ok(())
}

/// Runs `orcarein session list`, then returns.
fn run_session_list() -> Result<()> {
    let store = SessionStore::new().context("failed to locate session storage")?;
    let sessions = store.list().context("failed to list sessions")?;
    if sessions.is_empty() {
        println!("No saved sessions yet. ({})", store.dir().display());
        return Ok(());
    }
    println!("{:<15}  {:<9}  {:>5}  TITLE", "ID", "AGE", "TURNS");
    let now = SessionStore::now_ms();
    for SessionSummary {
        id,
        created_at_ms,
        turns,
        title,
    } in sessions
    {
        println!(
            "{:<15}  {:<9}  {:>5}  {}",
            id,
            format_age(now, created_at_ms),
            turns,
            title
        );
    }
    Ok(())
}

/// Outcome of resolving a user-typed id-or-prefix against existing session ids.
#[derive(Debug, PartialEq, Eq)]
enum IdMatch {
    /// Exactly one id matches (an exact full id, or a unique prefix).
    One(String),
    /// No id matches — caller reports "no such session".
    None,
    /// The prefix matches several ids — caller lists them and refuses.
    Many(Vec<String>),
}

/// Resolves `needle` (a full id or a short prefix) against `ids`. An exact
/// full-id match always wins — even when that id is also a prefix of a longer
/// one. Otherwise matches by `starts_with`: one hit → `One`, none → `None`,
/// several → `Many`. A blank needle matches nothing (so it never resumes or
/// deletes blindly). Pure, so the matching rules are unit-testable.
fn resolve_id_prefix(needle: &str, ids: &[String]) -> IdMatch {
    let needle = needle.trim();
    if needle.is_empty() {
        return IdMatch::None;
    }
    if ids.iter().any(|id| id == needle) {
        return IdMatch::One(needle.to_owned());
    }
    let hits: Vec<String> = ids
        .iter()
        .filter(|id| id.starts_with(needle))
        .cloned()
        .collect();
    match hits.len() {
        0 => IdMatch::None,
        1 => IdMatch::One(hits.into_iter().next().expect("len checked == 1")),
        _ => IdMatch::Many(hits),
    }
}

/// Maps a menu choice (a 1-based line the user typed) against the listed
/// sessions to the chosen session id. Blank, non-numeric, zero, and
/// out-of-range all mean "cancel" → `None`, so a bare Enter backs out safely.
/// Pure (no I/O) so the selection logic is unit-testable.
fn resolve_pick(input: &str, sessions: &[SessionSummary]) -> Option<String> {
    let n: usize = input.trim().parse().ok()?;
    if n == 0 || n > sessions.len() {
        return None;
    }
    Some(sessions[n - 1].id.clone())
}

/// Interactive `session resume` (no id): prints a numbered menu of saved
/// sessions and returns the chosen id. `Ok(None)` means "nothing to resume" or
/// "user cancelled" — the caller exits cleanly. Requires a tty: piped stdin has
/// no human to pick, so we refuse with a hint to pass the id explicitly.
fn pick_session() -> Result<Option<String>> {
    let store = SessionStore::new().context("failed to locate session storage")?;
    let sessions = store.list().context("failed to list sessions")?;
    if sessions.is_empty() {
        println!("No saved sessions yet. ({})", store.dir().display());
        return Ok(None);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "`session resume` without an id needs an interactive terminal — \
             pass the id explicitly (see `session list`)"
        );
    }

    println!(
        "{:<3}  {:<15}  {:<9}  {:>5}  TITLE",
        "#", "ID", "AGE", "TURNS"
    );
    let now = SessionStore::now_ms();
    for (i, s) in sessions.iter().enumerate() {
        println!(
            "{:<3}  {:<15}  {:<9}  {:>5}  {}",
            i + 1,
            s.id,
            format_age(now, s.created_at_ms),
            s.turns,
            s.title
        );
    }
    eprint!("选择要恢复的 session [1-{}，回车取消]: ", sessions.len());
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(None);
    }
    match resolve_pick(&line, &sessions) {
        Some(id) => Ok(Some(id)),
        None => {
            println!("已取消。");
            Ok(None)
        }
    }
}

/// What `session delete <needle>` should do, decided from the existing session
/// summaries (and, for the corrupt-file escape hatch, whether an exact-id file
/// is present on disk). Separated from I/O so the decision is unit-testable —
/// in particular the safety property: a non-matching needle is always
/// `NotFound`, never a present session.
#[derive(Debug, PartialEq, Eq)]
enum DeleteAction {
    /// Delete this resolved id (a valid session, or an exact-id corrupt file).
    Delete(String),
    /// Nothing matched — report it cleanly.
    NotFound,
    /// The prefix matched several ids — list them and refuse.
    Ambiguous(Vec<String>),
}

/// Pure decision for `session delete`. `exact_file_exists(id)` lets the
/// corrupt-but-present escape hatch be tested without touching the filesystem.
fn decide_delete(
    needle: &str,
    summaries: &[SessionSummary],
    exact_file_exists: impl Fn(&str) -> bool,
) -> DeleteAction {
    let ids: Vec<String> = summaries.iter().map(|s| s.id.clone()).collect();
    match resolve_id_prefix(needle, &ids) {
        IdMatch::One(id) => DeleteAction::Delete(id),
        IdMatch::Many(hits) => DeleteAction::Ambiguous(hits),
        IdMatch::None => {
            // Escape hatch: an exact id whose file `list` couldn't parse
            // (corrupt but present) is still prunable by full id.
            let exact = needle.trim();
            if !exact.is_empty() && exact_file_exists(exact) {
                DeleteAction::Delete(exact.to_owned())
            } else {
                DeleteAction::NotFound
            }
        }
    }
}

/// Runs `orcarein session delete <id-or-prefix>`, then returns. The argument is
/// a full id or an unambiguous prefix. Nothing matches → friendly message (not
/// an error); several match → list them and refuse; one match → delete it and
/// echo the title so you see what you pruned.
fn run_session_delete(needle: &str) -> Result<()> {
    let store = SessionStore::new().context("failed to locate session storage")?;
    let summaries = store.list().unwrap_or_default();

    match decide_delete(needle, &summaries, |id| store.path_for(id).exists()) {
        DeleteAction::Delete(id) => {
            let title = summaries
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "(无标题)".to_owned());
            store
                .delete(&id)
                .with_context(|| format!("failed to delete session '{id}'"))?;
            println!("已删除 session {id}（{title}）");
        }
        DeleteAction::Ambiguous(hits) => {
            println!("前缀 '{}' 匹配多个 session，请加长：", needle.trim());
            for id in &hits {
                let title = summaries
                    .iter()
                    .find(|s| &s.id == id)
                    .map(|s| s.title.as_str())
                    .unwrap_or("");
                println!("  {id}  {title}");
            }
        }
        DeleteAction::NotFound => {
            println!("没有这个 session：{needle}（用 `session list` 查看现有的）");
        }
    }
    Ok(())
}

/// Formats the gap between two Unix-ms timestamps as a coarse "N ago" string.
/// Pure integer math — no calendar crate needed.
fn format_age(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Initialises a `tracing` subscriber that writes to stderr and is **quiet
/// by default** (level `warn`). Opt into internal diagnostics with e.g.
/// `RUST_LOG=orcarein=debug`. Uses `try_init` so it is a no-op if a
/// subscriber is already installed (e.g. under a test harness).
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// On Unix, returns `Some(true)` iff the file's permission bits are exactly
/// `0600`. On non-Unix targets there is no POSIX mode, so returns `None`
/// (the report treats `None` as "not applicable").
#[cfg(unix)]
fn secrets_mode_0600(path: Option<&Path>) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path?).ok()?;
    Some(meta.permissions().mode() & 0o777 == 0o600)
}

#[cfg(not(unix))]
fn secrets_mode_0600(_path: Option<&Path>) -> Option<bool> {
    None
}

/// Runs `orcarein doctor`: gathers facts via real I/O, hands each to the
/// pure check functions in `orcarein_core::doctor`, prints the report, and
/// exits with a non-zero code if any check FAILed. Never returns.
fn run_doctor(cli: &Cli) -> ! {
    let mut checks: Vec<Check> = Vec::new();

    checks.push(doctor::build_info(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ));

    // config.toml
    let cfg_path = Config::config_path();
    let cfg_path_str = display_or_empty(cfg_path.as_deref());
    let cfg_exists = cfg_path.as_deref().map(Path::exists).unwrap_or(false);
    let cfg_parse_err = if cfg_exists {
        cfg_path
            .as_deref()
            .and_then(|p| Config::load_from(p).err())
            .map(|e| e.to_string())
    } else {
        None
    };
    checks.push(doctor::config_check(
        cfg_path.is_some(),
        &cfg_path_str,
        cfg_exists,
        cfg_parse_err.as_deref(),
    ));

    // secrets.toml
    let sec_path = SecretStore::secrets_path();
    let sec_path_str = display_or_empty(sec_path.as_deref());
    let sec_exists = sec_path.as_deref().map(Path::exists).unwrap_or(false);
    let sec_parse_err = if sec_exists {
        sec_path
            .as_deref()
            .and_then(|p| SecretStore::load_from(p).err())
            .map(|e| e.to_string())
    } else {
        None
    };
    let sec_mode = if sec_exists {
        secrets_mode_0600(sec_path.as_deref())
    } else {
        None
    };
    checks.push(doctor::secrets_check(
        sec_path.is_some(),
        &sec_path_str,
        sec_exists,
        sec_parse_err.as_deref(),
        sec_mode,
    ));

    // provider + API key (resolved the same way the REPL resolves them,
    // minus building the provider — doctor must not require a key)
    let config = Config::load().unwrap_or_default();
    let provider_name = non_blank(cli.provider.clone())
        .or_else(|| non_blank(std::env::var("ORCAREIN_PROVIDER").ok()))
        .or_else(|| non_blank(config.provider.clone()))
        .unwrap_or_else(|| "deepseek".to_owned());
    let known = matches!(provider_name.as_str(), "deepseek" | "openai");
    checks.push(doctor::provider_check(&provider_name, known));

    if known {
        let env_var = env_key_var(&provider_name).unwrap_or("");
        let from_env = std::env::var(env_var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
        let from_secrets = SecretStore::load()
            .ok()
            .and_then(|s| s.get(&provider_name).map(|k| !k.is_empty()))
            .unwrap_or(false);
        let (present, source) = if from_env {
            (true, Some(format!("env {env_var}")))
        } else if from_secrets {
            (true, Some("secrets.toml".to_owned()))
        } else {
            (false, None)
        };
        checks.push(doctor::api_key_check(
            &provider_name,
            present,
            source.as_deref(),
            env_var,
        ));
    }

    // data dir (sessions)
    let data_path = SessionStore::sessions_dir();
    let data_path_str = display_or_empty(data_path.as_deref());
    let (writable, count) = match data_path.as_deref() {
        Some(p) => {
            let w = std::fs::create_dir_all(p).is_ok();
            let c = SessionStore::new()
                .ok()
                .and_then(|s| s.list().ok())
                .map(|v| v.len());
            (w, c)
        }
        None => (false, None),
    };
    checks.push(doctor::data_dir_check(
        data_path.is_some(),
        &data_path_str,
        writable,
        count,
    ));

    // tools
    let allowlist = cli.tools.as_deref().or(config.tools_allowlist.as_deref());
    let registry = build_registry(allowlist);
    let names = registry.names();
    checks.push(doctor::tools_check(&names));

    print_doctor_report(&checks);

    let code = i32::from(doctor::worst_status(&checks) == CheckStatus::Fail);
    std::process::exit(code);
}

/// Formats an optional path for display, or `""` when absent.
fn display_or_empty(path: Option<&Path>) -> String {
    path.map(|p| p.display().to_string()).unwrap_or_default()
}

/// Prints the doctor report table + a one-line verdict.
fn print_doctor_report(checks: &[Check]) {
    println!("orcarein doctor\n");
    for c in checks {
        println!("[{:<4}] {:<10} {}", c.status.label(), c.name, c.detail);
    }
    let t = doctor::tally(checks);
    println!(
        "\n{} passed, {} warning(s), {} failure(s).",
        t.pass, t.warn, t.fail
    );
    match doctor::worst_status(checks) {
        CheckStatus::Fail => {
            println!("doctor: FAIL — fix the failures above before running OrcaRein.")
        }
        CheckStatus::Warn => println!("doctor: OK, with warnings."),
        CheckStatus::Pass => println!("doctor: all good."),
    }
}

/// Builds the tool registry honoring `--tools`. Warns to stderr if the
/// allowlist names any unknown tool.
/// The tool-call ceiling for a child `task` agent. Lower than the parent's
/// `MAX_TOOL_ITERATIONS` since a subagent handles one self-contained sub-task.
const MAX_SUBAGENT_ITERATIONS: usize = 12;

/// Augments `registry` with the `task` subagent tool. The child registry comes
/// from [`build_registry`] (built-ins only — never `task`), so a subagent cannot
/// spawn its own subagents (no recursion). The caller supplies a `policy_factory`
/// so each agent path mirrors its own permission posture for its children.
#[allow(clippy::too_many_arguments)]
fn register_subagent(
    registry: &mut ToolRegistry,
    provider: Arc<dyn Provider>,
    allowlist: Option<Vec<String>>,
    model: String,
    policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync>,
    ruleset_factory: Arc<dyn Fn() -> Ruleset + Send + Sync>,
    mode: SharedMode,
    hooks: HookSet,
) {
    let af = allowlist.clone();
    let registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync> =
        Arc::new(move || build_registry(af.as_deref()));
    registry.register(Box::new(SubagentTool::new(
        provider,
        registry_factory,
        policy_factory,
        ruleset_factory,
        mode,
        hooks,
        model,
        MAX_SUBAGENT_ITERATIONS,
        DEFAULT_SUBAGENT_PERSONA.to_string(),
    )));
}

fn build_registry(allowlist: Option<&[String]>) -> ToolRegistry {
    let all_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ReadFileTool),
        Box::new(WriteFileTool),
        Box::new(ListDirTool),
        Box::new(BashTool),
        Box::new(EditTool),
        Box::new(SearchTool),
    ];

    if let Some(list) = allowlist {
        let known: std::collections::HashSet<&str> = KNOWN_TOOLS.iter().copied().collect();
        for name in list {
            if !known.contains(name.as_str()) {
                eprintln!("warning: --tools listed unknown tool '{name}' (ignored)");
            }
        }
    }

    let allow_set: Option<std::collections::HashSet<String>> =
        allowlist.map(|list| list.iter().cloned().collect());

    let mut registry = ToolRegistry::new();
    for tool in all_tools {
        let keep = match &allow_set {
            Some(set) => set.contains(tool.name()),
            None => true,
        };
        if keep {
            registry.register(tool);
        }
    }
    registry
}

/// Loads the repo's AGENTS.md (walking up from `cwd`) and returns the
/// formatted block to append to a system prompt, or `None`. Logs via
/// `tracing` (silent by default) so headless stdout/stderr stay clean.
fn project_memory_block(cwd: &std::path::Path) -> Option<String> {
    let mem = orcarein_core::load_project_memory(cwd)?;
    tracing::info!(
        path = %mem.path.display(),
        bytes = mem.content.len(),
        truncated = mem.truncated,
        "loaded AGENTS.md"
    );
    Some(orcarein_core::format_memory_block(
        &mem.content,
        mem.truncated,
    ))
}

/// Base persona prompt + the project-memory block (if any). Used ONLY for a
/// fresh session — a resumed session keeps its frozen prompt, consistent with
/// how `config.system_prompt` already behaves.
fn fresh_session_prompt(base: String, cwd: &std::path::Path) -> String {
    match project_memory_block(cwd) {
        Some(block) => format!("{base}{block}"),
        None => base,
    }
}

/// Appends the discovered-skills index to a fresh-session prompt, if any. Kept
/// separate from `fresh_session_prompt` so the (session-fixed) skills index and
/// the (per-`/new`-refreshed) AGENTS.md block have independent lifecycles: the
/// index and the registered `SkillTool` are built from the same startup
/// discovery, so what the index lists is always loadable.
fn append_skills_index(prompt: String, index: Option<&str>) -> String {
    match index {
        Some(block) => format!("{prompt}\n\n{block}"),
        None => prompt,
    }
}

/// Whether `/init` may write AGENTS.md in `cwd`.
#[derive(Debug)]
enum InitDecision {
    /// cwd already has AGENTS.md — refuse (never clobber).
    Exists,
    /// No AGENTS.md anywhere up the tree — go ahead.
    Proceed,
    /// cwd has none but an ancestor does — writing here shadows it; warn + proceed.
    ProceedShadowing(std::path::PathBuf),
}

/// Pure decision for `/init` (no model call, no fs writes).
fn init_precondition(cwd: &std::path::Path) -> InitDecision {
    if cwd.join(orcarein_core::memory::AGENTS_FILENAME).is_file() {
        InitDecision::Exists
    } else if let Some(found) = orcarein_core::find_agents_md(cwd) {
        // cwd has none, so any hit is necessarily an ancestor's.
        InitDecision::ProceedShadowing(found)
    } else {
        InitDecision::Proceed
    }
}

/// `/compact`: summarize older turns to shrink the prompt. Returns whether it
/// compacted (so the caller can persist). Never propagates errors.
async fn handle_compact(provider: &dyn Provider, model: &str, session: &mut Session) -> bool {
    use orcarein_core::compact::{compact_session, CompactOutcome, KEEP_RECENT_USER_TURNS};
    match compact_session(session, provider, model, KEEP_RECENT_USER_TURNS).await {
        Ok(CompactOutcome::Compacted {
            messages_before,
            messages_after,
            chars_before,
            chars_after,
        }) => {
            println!(
                "已压缩：{messages_before}→{messages_after} 条（约 {chars_before}→{chars_after} 字符）。"
            );
            println!("（下次请求会一次性 cache miss——前缀已变；之后在更小前缀上重建，更省。）");
            true
        }
        Ok(CompactOutcome::NothingToDo) => {
            println!("nothing to compact（历史已经很小）。");
            false
        }
        Err(e) => {
            eprintln!("/compact 失败：{e:#}");
            false
        }
    }
}

/// `/init`: have the agent explore the repo and write AGENTS.md. Provider is a
/// parameter so this is testable with `MockProvider`. Never propagates errors
/// — the REPL must survive a failed command.
async fn handle_init(provider: &dyn Provider, model: &str, cwd: &std::path::Path, hooks: &HookSet) {
    match init_precondition(cwd) {
        InitDecision::Exists => {
            println!("AGENTS.md exists; delete it first to regenerate.");
            return;
        }
        InitDecision::ProceedShadowing(parent) => {
            println!(
                "note: a parent AGENTS.md at {} is currently active; \
                 creating one here will shadow it.",
                parent.display()
            );
        }
        InitDecision::Proceed => {}
    }

    // Read-only exploration + the single write. No bash/edit.
    let init_tools: Vec<String> = ["search", "read_file", "list_dir", "write_file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let registry = build_registry(Some(&init_tools));
    let tool_defs = registry.definitions();
    let agent = Agent::new(provider, &registry, &tool_defs)
        .with_max_iterations(ISSUE_MAX_ITERATIONS)
        .with_hooks(hooks.clone());

    // read_file is Safe (always allowed); allowlist the Risky read-only + write.
    let mut policy: Box<dyn PermissionPolicy> = Box::new(AllowlistPolicy::from_allowed([
        "search",
        "list_dir",
        "write_file",
    ]));

    let system = "You are OrcaRein, initializing AGENTS.md for this repository. \
        Explore the project with search, read_file, and list_dir, then write a concise \
        AGENTS.md at the repository root using write_file. Include sections: Project (one \
        line on purpose), Build & Test (the real commands), Conventions, and Layout. Write \
        only facts you verified from the repository. Keep it short. The working directory is \
        the repository root.";
    let mut session = Session::new(system);
    session.push_user("Initialize AGENTS.md for this repository.");

    let mut sink = IssueSink; // reuse the issue path's quiet, operator-facing sink
    match agent
        .run_turn(&mut session, model, policy.as_mut(), &mut sink)
        .await
    {
        Ok(outcome) => println!("\n{}", outcome.content.trim_end()),
        Err(e) => eprintln!("/init failed: {e:#}"),
    }
}

/// Terminal width + whether we can draw the fancy box. crossterm is tui-only,
/// so the non-tui build returns a safe default without referencing it. Width is
/// read through the single shared `overlay::term_cols` probe.
#[cfg(feature = "tui")]
fn header_env() -> (u16, bool) {
    use std::io::IsTerminal;
    let term = std::env::var("TERM").ok();
    let fancy = std::io::stdout().is_terminal() && overlay::overlay_capable(true, term.as_deref());
    (overlay::term_cols(), fancy)
}
#[cfg(not(feature = "tui"))]
fn header_env() -> (u16, bool) {
    (80, false)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    // Subcommands that fully short-circuit the REPL. `session resume` does NOT
    // short-circuit — it yields an id we carry into the REPL below.
    let mut resume_id: Option<String> = None;
    match cli.command.clone() {
        Some(Command::Config { action }) => return run_config(action),
        Some(Command::Session {
            action: SessionAction::List,
        }) => return run_session_list(),
        Some(Command::Session {
            action: SessionAction::Delete { id },
        }) => return run_session_delete(&id),
        Some(Command::Login {
            provider,
            no_verify,
        }) => return run_login(&cli, provider, no_verify).await,
        Some(Command::Doctor) => run_doctor(&cli), // diverges (process::exit)
        Some(Command::Run { prompt, allow }) => {
            let code = run_once(&cli, prompt, allow).await;
            std::process::exit(code);
        }
        Some(Command::Issue { number }) => {
            let code = run_issue(&cli, number).await;
            std::process::exit(code);
        }
        #[cfg(feature = "hardware")]
        Some(Command::Hw {
            action:
                HwAction::Monitor {
                    pins,
                    interval,
                    once,
                },
        }) => {
            let pins = if pins.is_empty() {
                DEFAULT_MONITOR_PINS.to_vec()
            } else {
                pins
            };
            return hwmon::run_monitor(&hwmon::DemoGpio, &pins, interval, once)
                .context("gpio monitor failed");
        }
        Some(Command::Session {
            action: SessionAction::Resume { id },
        }) => {
            // An explicit id is used as-is; without one, show the picker. A
            // cancelled or empty pick exits cleanly rather than starting a
            // fresh session the user didn't ask for.
            match id {
                Some(id) => resume_id = Some(id),
                None => match pick_session()? {
                    Some(id) => resume_id = Some(id),
                    None => return Ok(()),
                },
            }
        }
        None => {}
    }

    if cli.no_permission && std::io::stdin().is_terminal() {
        bail!(
            "--no-permission requires non-tty stdin (e.g. piped input) — refusing in interactive mode"
        );
    }

    // Resolve the session store and, if resuming, load the saved session *now* —
    // before `resolve()` demands an API key — so `resume <bad-id>` fails fast
    // with a "no such session" error rather than a confusing key error.
    let store = SessionStore::new().context("failed to locate session storage")?;
    let resumed = match resume_id {
        Some(needle) => {
            // Resolve a full id or an unambiguous prefix (the picker already
            // hands back a full id, which resolves to itself). Nothing or
            // several → fail fast with a clear message, before demanding a key.
            let ids: Vec<String> = store
                .list()
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.id)
                .collect();
            let id = match resolve_id_prefix(&needle, &ids) {
                IdMatch::One(id) => id,
                IdMatch::None => {
                    bail!("no such session '{needle}' — see `session list`")
                }
                IdMatch::Many(hits) => bail!(
                    "'{}' matches {} sessions — be more specific:\n{}",
                    needle.trim(),
                    hits.len(),
                    hits.iter()
                        .map(|h| format!("  {h}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            };
            let loaded = store
                .load(&id)
                .with_context(|| format!("failed to resume session '{id}'"))?;
            let created = store
                .created_at(&id)
                .unwrap_or_else(|_| SessionStore::now_ms());
            Some((loaded, id, created))
        }
        None => None,
    };

    // Everything up to the key gate below must work WITHOUT an API key: the
    // welcome box is drawn first, and only then do we ask for one.
    let Settings {
        provider: provider_name,
        mut model,
        system_prompt,
        tools_allowlist,
        permission_rules,
        hooks,
        retry,
        // Not yet consumed here — a follow-up cut wires this into a runtime
        // `SharedMode` for the gate/header. Keep it bound (not `..`) so the
        // compiler forces every future `Settings` field through this site.
        perm_mode: _perm_mode,
    } = resolve_settings(&cli)?;

    // Keep the base persona prompt (before AGENTS.md injection) so `/new` can
    // build a fresh session that re-reads the current project memory.
    let base_system = system_prompt.clone();

    // Discover project skills once at startup (cwd walk-up). Fixed for the
    // whole session so the index and the registered SkillTool stay consistent.
    let skills = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        orcarein_core::discover_skills(&cwd)
    };
    let skills_index: Option<String> = if skills.is_empty() {
        None
    } else {
        Some(orcarein_core::skills_index(&skills))
    };

    // Either continue the resumed session (keeping its id + creation time so
    // auto-save writes back to the same file) or start a fresh one. `mut` so
    // `/model` and `/resume`/`/new` can switch them at runtime.
    let (mut session, mut session_id, mut created_at_ms) = match resumed {
        Some((loaded, id, created)) => {
            println!("Resumed session {id} ({} turns).", loaded.turn_count());
            (loaded, id, created)
        }
        None => {
            let created = SessionStore::now_ms();
            // Inject project memory into the fresh-session prompt only. A resumed
            // session keeps its frozen prompt (consistent with config.system_prompt),
            // so a changed AGENTS.md takes effect on the next new session.
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let prompt = append_skills_index(
                fresh_session_prompt(system_prompt, &cwd),
                skills_index.as_deref(),
            );
            (Session::new(&prompt), created.to_string(), created)
        }
    };

    // The key gate's *judgment* runs here — before the welcome box — so a
    // `Bail` (no key, nobody to ask) can fail the way it always has: one error
    // line, no chrome. `Proceed`/`Prompt` still draw the box below first; only
    // `Prompt`'s actual login prompt happens after it. That box-before-prompt
    // ordering is the deliberate part of this design, not a bug to fix.
    let secrets = SecretStore::load().context("failed to load secrets.toml")?;
    let stored = secrets.resolve(&provider_name);
    let tty = {
        use std::io::IsTerminal;
        interactive(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        )
    };
    let first_run = first_run_decision(stored.as_deref(), tty);
    if first_run == FirstRun::Bail {
        // No key and nobody to ask: build_provider(_, None, _) always errors
        // here and owns the message/exit code, unchanged by this cut — it's
        // just reached before the box now instead of after it.
        build_provider(&provider_name, None, retry.clone())?;
    }

    // Terminal width + capability, resolved once and reused by the header, the
    // streaming sink, the permission prompt, and /help. `fancy` gates the boxed
    // chrome; `mode` is the color capability (None on non-tty / NO_COLOR / dumb,
    // so a NO_COLOR tty still draws the box, just uncolored).
    let (cols, fancy) = header_env();
    let mode = color::detect(fancy);

    {
        use header::{header_ansi, render_header, status_line, HeaderModel};
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let hm = HeaderModel {
            title: "OrcaRein",
            model: &model,
            provider: &provider_name,
            cwd: header::abbreviate_home(&cwd),
            session: header::short_id(&session_id),
            saved: true,
            tips: vec![
                ("/help", "命令一览"),
                ("/init", "初始化"),
                ("/compact", "压缩上下文"),
            ],
        };
        #[cfg(feature = "tui")]
        if fancy {
            for line in whale::whale_banner(mode, cols) {
                println!("{line}");
            }
        }
        for line in header_ansi(&render_header(&hm, cols, fancy), mode) {
            println!("{line}");
        }
        if let Some(s) = status_line(cli.no_permission, cli.no_economy) {
            println!("{}", color::paint(mode, color::Token::Warning, &s));
        }
    }

    // The key gate. The judgment (`first_run`) already ran above, before the
    // box — a `Bail` returned from there, so only `Proceed`/`Prompt` reach here.
    let (api_key, models_hint) = match first_run {
        FirstRun::Proceed => (stored, None),
        FirstRun::Prompt => match first_run_login(&provider_name).await? {
            Some((key, hint)) => (Some(key), Some(hint)),
            None => {
                // The user declined. Not an error — say how to come back, exit 0.
                let var = env_key_var(&provider_name).expect("provider validated above");
                println!();
                println!(
                    "未设置 API key。随时可以运行 'orcarein login'，或设置 $env:{var} = '<your-key>'。"
                );
                return Ok(());
            }
        },
        // Reachable only if a future keyless provider makes `build_provider(_,
        // None, _)` succeed instead of erroring above — see the comment on that
        // early-return. Falling through to the normal provider build below (with
        // no prefetched model list) is the correct behavior for that day, not a
        // bug to guard against.
        FirstRun::Bail => (None, None),
    };
    let provider = build_provider(&provider_name, api_key, retry)?;

    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;

    // Resolve the selectable model list once (used by `/model` validation and the
    // picker popup). A short timeout keeps a slow/offline endpoint from stalling
    // startup. NOT tui-gated — validation reads it on the non-tui path too.
    //
    // Reuse the list the login's /v1/models call already fetched; only hit the
    // network again when we don't have one. When the login already learned we're
    // offline, don't spend another 2s timeout re-confirming it, and don't repeat
    // the "couldn't fetch" news — the user was just told.
    let (mut model_choices, models_fell_back) = match models_hint {
        Some(ModelsHint::Fetched(v)) if !v.is_empty() => (v, false),
        Some(ModelsHint::Offline) => (fallback_models(provider.name(), &model), false),
        None | Some(ModelsHint::Fetched(_)) => {
            use std::time::Duration;
            match tokio::time::timeout(Duration::from_secs(2), provider.list_models()).await {
                Ok(Ok(v)) if !v.is_empty() => (v, false),
                _ => (fallback_models(provider.name(), &model), true),
            }
        }
    };
    ensure_current_present(&mut model_choices, &model);

    if models_fell_back {
        println!(
            "{}",
            color::paint(mode, color::Token::Dim, "· 模型列表拉取失败，使用内置列表")
        );
    }
    println!();

    let mut registry = build_registry(tools_allowlist.as_deref());
    #[cfg(feature = "mcp")]
    let _mcp_clients = {
        let mcp_cfg = orcarein_core::Config::load().unwrap_or_default();
        orcarein_core::mcp::setup_servers(&mcp_cfg.mcp_servers, &mut registry).await
    };

    // Register the `task` subagent tool. Its children mirror the REPL's own
    // permission posture: interactive (with a subagent-tagged prompt) unless
    // `--no-permission`, in which case the child runs unprompted too. Clone the
    // provider Arc into the tool *before* `Agent::new` borrows `&*provider`.
    {
        let no_permission = cli.no_permission;
        let policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> =
            Arc::new(move || {
                if no_permission {
                    Box::new(AllowlistPolicy::allow_all())
                } else {
                    Box::new(InteractivePolicy::new(fancy, mode).with_subagent_prefix())
                }
            });
        register_subagent(
            &mut registry,
            Arc::clone(&provider),
            tools_allowlist.clone(),
            model.clone(),
            policy_factory,
            // Default placeholders — the REPL's own `perm_mode`/ruleset are
            // wired to the child in a later cut.
            Arc::new(Ruleset::with_defaults),
            SharedMode::new(PermissionMode::Default),
            hooks.clone(),
        );
    }
    if !skills.is_empty() {
        registry.register(Box::new(SkillTool::new(skills.clone())));
    }
    let tool_defs = registry.definitions();

    #[cfg(feature = "tui")]
    whale::swim_once(mode, cols).await;

    // The agent loop now lives in `orcarein-core`; the REPL is a thin frontend
    // that supplies an interactive permission policy and a printing event sink.
    let agent = Agent::new(provider.as_ref(), &registry, &tool_defs)
        .with_cache_mode(cache_mode(&cli))
        .with_ruleset(Ruleset::from_config(permission_rules))
        .with_hooks(hooks.clone());
    let mut policy: Box<dyn PermissionPolicy> = if cli.no_permission {
        Box::new(AllowlistPolicy::allow_all())
    } else {
        Box::new(InteractivePolicy::new(fancy, mode))
    };

    // The prompt-token count of the most recent turn ≈ current context fill;
    // surfaced in the per-turn meter and `/usage`. 0 until the first turn.
    let mut last_prompt_tokens: u64 = 0;

    #[cfg(feature = "tui")]
    let mut history: modal::History = Vec::new();

    loop {
        let line = {
            #[cfg(feature = "tui")]
            {
                use std::io::IsTerminal;
                let is_tty = std::io::stdout().is_terminal();
                let term = std::env::var("TERM").ok();
                if crate::overlay::overlay_capable(is_tty, term.as_deref()) {
                    // A persistent (per-input) context readout for the modal status
                    // bar: current fill as of the last turn, colored by threshold.
                    let ctx_label: Option<(String, color::Token)> = cost::context_window(&model)
                        .map(|w| {
                            let pct = if w > 0 {
                                last_prompt_tokens as f64 / w as f64 * 100.0
                            } else {
                                0.0
                            };
                            (format!("ctx {pct:.0}%"), ctx_token(pct))
                        });
                    match modal::modal_readline(
                        "> ",
                        &history,
                        ctx_label,
                        Some(short_model(&model)),
                        &model_choices,
                    ) {
                        Ok(modal::ReadOutcome::Submitted(s)) => s,
                        Ok(modal::ReadOutcome::Cancelled) => continue,
                        Ok(modal::ReadOutcome::Eof) => break,
                        Err(e) => return Err(e).context("modal editor failed"),
                    }
                } else {
                    match editor.readline("> ") {
                        Ok(line) => line,
                        Err(ReadlineError::Interrupted) => continue,
                        Err(ReadlineError::Eof) => break,
                        Err(e) => return Err(e).context("line editor failed"),
                    }
                }
            }
            #[cfg(not(feature = "tui"))]
            {
                match editor.readline("> ") {
                    Ok(line) => line,
                    Err(ReadlineError::Interrupted) => continue,
                    Err(ReadlineError::Eof) => break,
                    Err(e) => return Err(e).context("line editor failed"),
                }
            }
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        if let Some(stripped) = input.strip_prefix('/') {
            match handle_command(
                stripped,
                &mut session,
                &store,
                &session_id,
                created_at_ms,
                &model,
                last_prompt_tokens,
                &registry.names(),
                &skills,
                mode,
            ) {
                CommandAction::Continue => continue,
                CommandAction::Quit => break,
                CommandAction::RunInit => {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    handle_init(provider.as_ref(), &model, &cwd, &hooks).await;
                    continue;
                }
                CommandAction::RunCompact => {
                    if handle_compact(provider.as_ref(), &model, &mut session).await {
                        let _ = store.save(&session_id, created_at_ms, &session);
                    }
                    continue;
                }
                CommandAction::SwitchModel(n) => {
                    // The popup accepts insert a trailing space; trim so the id matches.
                    let new = expand_model_alias(n.trim(), provider.name());
                    if !is_known_model(&new, &model_choices) {
                        let opts = model_choices.join(" / ");
                        eprintln!("未知 model「{}」。可用：{opts}", n.trim());
                    } else if new == model {
                        println!("已经是 model {model} 了。");
                    } else {
                        model = new;
                        println!("已切换 model → {model}");
                        println!("（下次请求 cache 一次性 miss：前缀缓存按 model 隔离。）");
                        // Persist the choice so the next session resolves + shows it.
                        match Config::config_path() {
                            Some(path) => {
                                if let Err(e) = persist_model_choice(&path, &model) {
                                    eprintln!("（model 已切换，但未能写入 config.toml：{e}）");
                                }
                            }
                            None => eprintln!("（model 已切换，但无 config 目录可持久化）"),
                        }
                    }
                    continue;
                }
                CommandAction::SwitchSession(needle) => {
                    // The current session auto-saves each turn; save once more to be
                    // safe before swapping the live state.
                    let _ = store.save(&session_id, created_at_ms, &session);
                    let ids: Vec<String> = store
                        .list()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| s.id)
                        .collect();
                    match resolve_id_prefix(&needle, &ids) {
                        IdMatch::One(id) => match store.load(&id) {
                            Ok(loaded) => {
                                let created = store
                                    .created_at(&id)
                                    .unwrap_or_else(|_| SessionStore::now_ms());
                                let turns = loaded.turn_count();
                                session = loaded;
                                session_id = id.clone();
                                created_at_ms = created;
                                last_prompt_tokens = 0;
                                println!("已切到 session {id}（{turns} turns）。");
                            }
                            Err(e) => eprintln!("加载 session 失败：{e}"),
                        },
                        IdMatch::None => {
                            eprintln!("没有这个 session：{}（用 /sessions 看列表）", needle.trim())
                        }
                        IdMatch::Many(hits) => {
                            eprintln!("前缀 '{}' 匹配多个，请加长：", needle.trim());
                            for h in &hits {
                                eprintln!("  {h}");
                            }
                        }
                    }
                    continue;
                }
                CommandAction::NewSession => {
                    // Save the current session, then swap in a fresh one (new id =
                    // creation timestamp, project memory re-read).
                    let _ = store.save(&session_id, created_at_ms, &session);
                    let created = SessionStore::now_ms();
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    let prompt = append_skills_index(
                        fresh_session_prompt(base_system.clone(), &cwd),
                        skills_index.as_deref(),
                    );
                    session = Session::new(&prompt);
                    session_id = created.to_string();
                    created_at_ms = created;
                    last_prompt_tokens = 0;
                    println!("已新建 session {session_id}（空白对话）。");
                    continue;
                }
                CommandAction::Swim => {
                    // Fresh width, not the one captured at startup: after a
                    // resize, frames built for the old width wrap, and the
                    // animator's relative cursor moves would chew the scrollback.
                    #[cfg(feature = "tui")]
                    whale::swim_once(mode, overlay::term_cols()).await;
                    #[cfg(not(feature = "tui"))]
                    println!("此构建未编译 tui，鲸鱼游不动。");
                    continue;
                }
            }
        }

        let _ = editor.add_history_entry(input);
        #[cfg(feature = "tui")]
        history.push(input.to_string());
        // Expand `@path` mentions into file/directory blocks for the model. The
        // original `input` (with `@path`) is kept for history/display above; only
        // the message sent to the model carries the expanded content. The blocks
        // land at the tail of the latest user turn, so the cached prefix (system +
        // prior turns) is untouched.
        const DIR_CAP: usize = 500;
        const BIG_CAP: usize = 5000;
        // One gitignore-correct repo walk (from cwd), only when there's an @.
        let mention_cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let all_files = if input.contains('@') {
            orcarein_core::mention::list_project_files(&mention_cwd, BIG_CAP, false)
        } else {
            Vec::new()
        };
        let expanded = orcarein_core::mention::expand_mentions(input, |p| {
            let path = std::path::Path::new(p);
            if path.is_dir() {
                // Recursive file tree = repo file list filtered to this subtree.
                let base = p.trim_end_matches('/');
                let prefix = format!("{base}/");
                let tree: Vec<String> = all_files
                    .iter()
                    .filter(|f| f.starts_with(&prefix))
                    .take(DIR_CAP)
                    .cloned()
                    .collect();
                Some(orcarein_core::mention::Resolved::Dir(tree))
            } else {
                std::fs::read_to_string(path)
                    .ok()
                    .map(|s| orcarein_core::mention::Resolved::File(cap_chars(&s, 32 * 1024)))
            }
        });
        session.push_user(expanded);

        let mut sink = ReplSink::new(fancy, mode);
        match agent
            .run_turn(&mut session, &model, policy.as_mut(), &mut sink)
            .await
        {
            Ok(outcome) => {
                println!(); // close the final streamed line
                let total = session.usage();
                last_prompt_tokens = outcome.usage.prompt_tokens;
                let ctx_raw = cost::context_line(last_prompt_tokens, &model);
                let meter_raw = cost::meter_line(&total, &model);
                let turn = outcome.usage.total_tokens;
                let tot = total.total_tokens;
                if mode == color::ColorMode::None {
                    let ctx = ctx_raw.map(|c| format!(" | {c}")).unwrap_or_default();
                    let meter = meter_raw.map(|m| format!(" | {m}")).unwrap_or_default();
                    eprintln!("[tokens: +{turn} this turn / {tot} total{ctx}{meter}]\n");
                } else {
                    use color::Token;
                    let mut l = format!(
                        "  {}",
                        color::paint(mode, Token::Accent, &format!("+{turn} tok this turn"))
                    );
                    l.push_str(&color::paint(mode, Token::Dim, &format!(" · {tot} total")));
                    // ctx colored by threshold (≥50% warn / ≥80% err).
                    if let Some(c) = ctx_colored(last_prompt_tokens, &model, mode) {
                        l.push_str(&color::paint(mode, Token::Dim, " · "));
                        l.push_str(&c);
                    }
                    if let Some(m) = meter_raw {
                        l.push_str(&color::paint(mode, Token::Dim, &format!(" · {m}")));
                    }
                    eprintln!("{l}\n");
                }
                // Auto-save after a successful turn; never let a save error
                // interrupt the conversation.
                if let Err(e) = store.save(&session_id, created_at_ms, &session) {
                    eprintln!("[warn] 自动保存失败：{e}");
                }
            }
            Err(e) => {
                let msg = format!("[错误] {e:#}");
                eprintln!("\n{}\n", color::paint(mode, color::Token::Error, &msg));
                session.pop_last();
            }
        }
    }

    println!("再见。");
    Ok(())
}

/// The interactive permission policy: prompts the human on stdin/stderr and
/// caches sticky decisions for the session. The *non-interactive* policy
/// ([`AllowlistPolicy`]) lives in `orcarein-core` since it needs no I/O.
struct InteractivePolicy {
    store: PermissionStore,
    fancy: bool,
    mode: color::ColorMode,
    /// When set, the prompt is visibly marked as a subagent's request so it is
    /// not confused with the parent agent's own permission prompt.
    subagent: bool,
    /// User config path for persisting bash always-allow rules. `None` = never
    /// persist (sub-agent policies never write the user's global config).
    config_path: Option<std::path::PathBuf>,
}

impl InteractivePolicy {
    fn new(fancy: bool, mode: color::ColorMode) -> Self {
        InteractivePolicy {
            store: PermissionStore::new(),
            fancy,
            mode,
            subagent: false,
            config_path: Config::config_path(), // top-level persists
        }
    }

    /// Marks this policy as belonging to a subagent — its prompt is prefixed
    /// with "subagent 请求：" to distinguish it from the parent's.
    fn with_subagent_prefix(mut self) -> Self {
        self.subagent = true;
        self.config_path = None; // sub-agent must not write user config
        self
    }
}

impl PermissionPolicy for InteractivePolicy {
    fn decide(&mut self, tool: &str, args: &str, _risk: RiskLevel) -> Decision {
        let scope = scope_key(tool, args);
        if let Some(d) = self.store.cached(&scope) {
            return d;
        }
        let d = prompt_permission(tool, args, self.fancy, self.mode, self.subagent);
        if d.is_sticky() {
            self.store.remember(&scope, d);
        }
        // Persist ONLY a bash always-allow, as a targeted command rule.
        if d == Decision::AllowAlways && tool == "bash" {
            if let Some(path) = self.config_path.clone() {
                persist_bash_rule(&path, args);
            }
        }
        d
    }
}

/// Session-cache scope for a tool call: bash is keyed per-command (so "always
/// allow git status" doesn't allow all bash); every other tool by name.
fn scope_key(tool: &str, args: &str) -> String {
    if tool == "bash" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
            if let Some(c) = v.get("command").and_then(|x| x.as_str()) {
                return format!("bash\u{0}{c}");
            }
        }
    }
    tool.to_string()
}

/// Append a targeted `Bash(<command>)` allow rule to the user's config.toml.
/// Best-effort: a write failure is reported but never fails the decision.
/// If the config file exists but fails to parse, persistence is skipped
/// entirely rather than overwriting it with a fresh default (which would
/// silently drop provider/model/system_prompt/mcp_servers settings).
fn persist_bash_rule(config_path: &std::path::Path, args: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return;
    };
    let Some(cmd) = v.get("command").and_then(|x| x.as_str()) else {
        return;
    };
    let mut cfg = if config_path.exists() {
        match Config::load_from(config_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("orcarein: not persisting permission rule (config unreadable): {e}");
                return;
            }
        }
    } else {
        Config::default()
    };
    let rule = PermissionRule {
        tool: "bash".to_string(),
        command: Some(cmd.to_string()),
        path: None,
        action: RuleAction::Allow,
    };
    let perms = cfg
        .permissions
        .get_or_insert_with(PermissionConfig::default);
    if !perms.rules.contains(&rule) {
        perms.rules.push(rule);
    }
    if let Err(e) = cfg.save_to(config_path) {
        eprintln!("orcarein: could not persist permission rule: {e}");
    }
}

/// Renders [`AgentEvent`]s the way the REPL always has: reasoning/content to
/// stdout under `[思考]`/`[回复]` headers, tool activity to stderr.
struct ReplSink {
    started_reasoning: bool,
    started_content: bool,
    /// Whether to draw the colored `▌` chrome (a capable tty); falls back to the
    /// plain `[思考]/[回复]/[tool]` bracket labels otherwise (grep / redirect
    /// friendly). `mode` is the color capability — `None` keeps the chrome but
    /// drops the color (a NO_COLOR tty).
    fancy: bool,
    mode: color::ColorMode,
}

impl ReplSink {
    fn new(fancy: bool, mode: color::ColorMode) -> Self {
        ReplSink {
            started_reasoning: false,
            started_content: false,
            fancy,
            mode,
        }
    }

    fn head_reasoning(&self) {
        if self.fancy {
            println!("{}", color::paint(self.mode, color::Token::Dim, "▌ 思考"));
        } else {
            println!("[思考]");
        }
    }

    fn head_content(&self) {
        if self.fancy {
            println!(
                "{}{}",
                color::paint(self.mode, color::Token::Brand, "▌ "),
                color::paint(self.mode, color::Token::OrcaWhite, "回复"),
            );
        } else {
            println!("[回复]");
        }
    }
}

impl EventSink for ReplSink {
    fn emit(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Reasoning(text) => {
                if !self.started_reasoning {
                    self.head_reasoning();
                    self.started_reasoning = true;
                }
                // The "thinking" body is secondary — dimmed on a capable tty.
                if self.fancy {
                    print!("{}", color::paint(self.mode, color::Token::Dim, &text));
                } else {
                    print!("{text}");
                }
                let _ = std::io::stdout().flush();
            }
            AgentEvent::Content(text) => {
                if !self.started_content {
                    if self.started_reasoning {
                        println!("\n");
                    }
                    self.head_content();
                    self.started_content = true;
                }
                // The answer body stays the terminal default foreground.
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ToolStarted {
                name, arguments, ..
            } => {
                if self.started_content || self.started_reasoning {
                    println!();
                }
                if self.fancy {
                    eprintln!(
                        "{}{}{}",
                        color::paint(self.mode, color::Token::Accent, "  → "),
                        color::paint(self.mode, color::Token::Fg, &name),
                        color::paint(self.mode, color::Token::Dim, &format!("({arguments})")),
                    );
                } else {
                    eprintln!("[tool: {name}({arguments})]");
                }
            }
            AgentEvent::ToolFinished {
                result, is_error, ..
            } => {
                match (self.fancy, is_error) {
                    (true, true) => eprintln!(
                        "{}{}",
                        color::paint(self.mode, color::Token::Dim, "  └ "),
                        color::paint(self.mode, color::Token::Error, &format!("error · {result}")),
                    ),
                    (true, false) => eprintln!(
                        "{}{}",
                        color::paint(self.mode, color::Token::Dim, "  └ "),
                        color::paint(
                            self.mode,
                            color::Token::Success,
                            &format!("ok · {} bytes", result.len()),
                        ),
                    ),
                    (false, true) => eprintln!("[tool error] {result}"),
                    (false, false) => eprintln!("[result] {} bytes", result.len()),
                }
                // The next model response is a fresh segment.
                self.started_reasoning = false;
                self.started_content = false;
            }
            AgentEvent::Usage(_) => {} // printed once at end of turn
            AgentEvent::IterationLimit => {
                let msg = format!("[超过 tool call 上限 {MAX_TOOL_ITERATIONS} 次，中断]");
                if self.fancy {
                    eprintln!("{}", color::paint(self.mode, color::Token::Error, &msg));
                } else {
                    eprintln!("{msg}");
                }
            }
        }
    }
}

/// Machine-facing sink for `orcarein run`: keeps **stdout clean** (the final
/// answer is printed by [`run_once`], not streamed here) and routes tool
/// activity + warnings to stderr.
struct MachineSink;

impl EventSink for MachineSink {
    fn emit(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ToolStarted {
                name, arguments, ..
            } => eprintln!("[tool: {name}({arguments})]"),
            AgentEvent::ToolFinished {
                result, is_error, ..
            } => {
                if is_error {
                    eprintln!("[tool error] {result}");
                } else {
                    eprintln!("[tool ok] {} bytes", result.len());
                }
            }
            AgentEvent::IterationLimit => eprintln!("[warning: stopped at tool-iteration limit]"),
            // Reasoning / Content / Usage are diagnostics here: the final
            // answer comes from `TurnOutcome.content`, so stdout stays clean.
            _ => {}
        }
    }
}

/// Reads the prompt for `orcarein run`: the argument if given, else stdin.
fn read_prompt(arg: Option<String>) -> Result<String> {
    if let Some(p) = arg {
        if !p.trim().is_empty() {
            return Ok(p);
        }
    }
    if std::io::stdin().is_terminal() {
        bail!(
            "no prompt given — pass it as an argument (orcarein run \"...\") or pipe it on stdin"
        );
    }
    let buf =
        std::io::read_to_string(std::io::stdin()).context("failed to read prompt from stdin")?;
    if buf.trim().is_empty() {
        bail!("empty prompt on stdin");
    }
    Ok(buf)
}

/// Maps the `--no-economy` flag to a [`CacheMode`].
fn cache_mode(cli: &Cli) -> CacheMode {
    if cli.no_economy {
        CacheMode::Benchmark
    } else {
        CacheMode::Economy
    }
}

/// Runs one task non-interactively and returns a process exit code:
/// `0` success, `1` error, `2` stopped at the tool-iteration limit.
async fn run_once(cli: &Cli, prompt_arg: Option<String>, allow: Option<Vec<String>>) -> i32 {
    // Validate the prompt first so a headless caller that forgot it gets a
    // clear error before we ever demand an API key.
    let prompt = match read_prompt(prompt_arg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("orcarein: {e:#}");
            return 1;
        }
    };

    let resolved = match resolve(cli) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("orcarein: {e:#}");
            return 1;
        }
    };
    let Resolved {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        permission_rules,
        hooks,
        ..
    } = resolved;

    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let skills = orcarein_core::discover_skills(&cwd);
    let skills_index = if skills.is_empty() {
        None
    } else {
        Some(orcarein_core::skills_index(&skills))
    };

    #[cfg_attr(not(feature = "mcp"), allow(unused_mut))]
    let mut registry = build_registry(tools_allowlist.as_deref());
    #[cfg(feature = "mcp")]
    let _mcp_clients = {
        let mcp_cfg = orcarein_core::Config::load().unwrap_or_default();
        orcarein_core::mcp::setup_servers(&mcp_cfg.mcp_servers, &mut registry).await
    };

    // Register the `task` subagent tool with a policy_factory mirroring this
    // headless run's posture (allow_all / allowlist / deny_all). Clone the
    // provider Arc into the tool before `Agent::new` borrows `&*provider`.
    {
        let no_permission = cli.no_permission;
        let allow_child = allow.clone();
        let policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> =
            Arc::new(move || {
                if no_permission {
                    Box::new(AllowlistPolicy::allow_all())
                } else if let Some(names) = allow_child.clone() {
                    Box::new(AllowlistPolicy::from_allowed(names))
                } else {
                    Box::new(AllowlistPolicy::deny_all())
                }
            });
        register_subagent(
            &mut registry,
            Arc::clone(&provider),
            tools_allowlist.clone(),
            model.clone(),
            policy_factory,
            // Default placeholders — this headless run's own ruleset/mode are
            // wired to the child in a later cut.
            Arc::new(Ruleset::with_defaults),
            SharedMode::new(PermissionMode::Default),
            hooks.clone(),
        );
    }
    if !skills.is_empty() {
        registry.register(Box::new(SkillTool::new(skills)));
    }
    let tool_defs = registry.definitions();
    let agent = Agent::new(provider.as_ref(), &registry, &tool_defs)
        .with_cache_mode(cache_mode(cli))
        .with_ruleset(Ruleset::from_config(permission_rules))
        .with_hooks(hooks);
    if cli.no_economy {
        eprintln!("orcarein: economy OFF (benchmark — prefix cache defeated)");
    }

    let mut policy: Box<dyn PermissionPolicy> = if cli.no_permission {
        eprintln!("orcarein: warning: --no-permission runs ALL tools without prompting");
        Box::new(AllowlistPolicy::allow_all())
    } else if let Some(names) = allow {
        Box::new(AllowlistPolicy::from_allowed(names))
    } else {
        Box::new(AllowlistPolicy::deny_all())
    };

    // Inject project memory (AGENTS.md) into the headless run prompt too.
    // Reuses the `cwd` bound above for skill discovery.
    let system_prompt = append_skills_index(
        fresh_session_prompt(system_prompt, &cwd),
        skills_index.as_deref(),
    );
    let mut session = Session::new(&system_prompt);
    session.push_user(&prompt);

    let mut sink = MachineSink;
    match agent
        .run_turn(&mut session, &model, policy.as_mut(), &mut sink)
        .await
    {
        Ok(outcome) => {
            // stdout = the final answer only.
            println!("{}", outcome.content.trim_end());
            let meter = cost::meter_line(&outcome.usage, &model)
                .map(|m| format!(" | {m}"))
                .unwrap_or_default();
            eprintln!(
                "orcarein: [tokens: {} total{}]",
                outcome.usage.total_tokens, meter
            );
            if outcome.hit_iteration_limit {
                eprintln!("orcarein: warning: stopped at the tool-iteration limit");
                return 2;
            }
            0
        }
        Err(e) => {
            eprintln!("orcarein: {e:#}");
            1
        }
    }
}

/// Synchronously prompts the user. Any input we cannot parse — empty
/// line, EOF, IO error — collapses to `DenyOnce` (deny-by-default). On a capable
/// tty it draws the colored `▌ 权限确认` chrome; otherwise a plain one-liner.
fn prompt_permission(
    name: &str,
    args: &str,
    fancy: bool,
    mode: color::ColorMode,
    subagent: bool,
) -> Decision {
    use color::Token;
    // Subagent prompts are visibly tagged so they are not mistaken for the
    // parent agent's own request.
    let who = if subagent { "subagent 请求：" } else { "" };
    if fancy {
        let p = |t: Token, s: &str| color::paint(mode, t, s);
        eprintln!();
        eprintln!(
            "{}{}",
            p(Token::Warning, "▌ 权限确认"),
            p(Token::Dim, &format!("  {who}{name} 请求授权"))
        );
        eprintln!();
        eprintln!("   {}", p(Token::Fg, &format!("{name}({args})")));
        eprintln!();
        // Option keys colored by risk; the default (deny-once) is reverse-marked.
        let opts = format!(
            "{}  {} {}   {} {}   {} {}   {} {}   {}",
            p(Token::Dim, "允许？"),
            p(Token::Success, "[y]"),
            p(Token::Dim, "本次"),
            p(Token::Accent, "[a]"),
            p(Token::Dim, "总是"),
            color::reverse("[n]"),
            p(Token::Dim, "拒绝"),
            p(Token::Error, "[N]"),
            p(Token::Dim, "永不"),
            p(Token::Warning, "←默认"),
        );
        eprint!("   {opts} ");
    } else {
        eprintln!();
        eprintln!("{who}OrcaRein wants to run: {name}({args})");
        eprint!("Allow? [y=once N=never a=always n=once]: ");
    }
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Decision::DenyOnce;
    }
    match line.trim().chars().next() {
        Some('y') | Some('Y') => Decision::AllowOnce,
        Some('a') | Some('A') => Decision::AllowAlways,
        Some('N') => Decision::DenyAlways,
        _ => Decision::DenyOnce,
    }
}

/// Build the `/history` pager doc: each message becomes a colored role bar plus
/// its content rendered as Markdown (tool calls/results stay plain). `tui` only —
/// the headless build uses the plain-text [`render_transcript`] instead.
#[cfg(feature = "tui")]
fn history_doc(
    session: &Session,
    width: u16,
    mode: color::ColorMode,
) -> Vec<overlay::RenderedLine> {
    // Role bars use the boolean `rgb`; Markdown bodies tier per `mode`.
    let rgb = color::use_rgb(mode);
    let mut doc: Vec<overlay::RenderedLine> = Vec::new();
    let blank = |d: &mut Vec<overlay::RenderedLine>| d.push(overlay::styled_line("", rgb));
    for m in session.messages() {
        match m.role.as_str() {
            "system" => continue, // not part of the visible chat
            "user" => {
                doc.push(overlay::styled_line("▌ 你", rgb));
                doc.extend(markdown::render(m.content.trim_end(), width, mode, false));
                blank(&mut doc);
            }
            "assistant" => {
                for tc in &m.tool_calls {
                    doc.push(overlay::styled_line(
                        &format!(
                            "▌ OrcaRein → {}({})",
                            tc.function.name, tc.function.arguments
                        ),
                        rgb,
                    ));
                }
                if !m.content.trim().is_empty() {
                    doc.push(overlay::styled_line("▌ OrcaRein", rgb));
                    doc.extend(markdown::render(m.content.trim_end(), width, mode, false));
                }
                blank(&mut doc);
            }
            "tool" => {
                doc.push(overlay::styled_line("▌ 工具结果", rgb));
                for line in m.content.trim_end().split('\n') {
                    doc.push(overlay::styled_line(line, rgb));
                }
                blank(&mut doc);
            }
            other => {
                doc.push(overlay::styled_line(&format!("▌ {other}"), rgb));
                doc.extend(markdown::render(m.content.trim_end(), width, mode, false));
                blank(&mut doc);
            }
        }
    }
    if doc.is_empty() {
        doc.push(overlay::styled_line("(对话为空)", rgb));
    }
    doc
}

/// Renders a session's messages to plain text for the `/history` pager. Pure
/// and read-only — viewing history must never *become* history: the persisted
/// `Vec<Message>` (and thus the model's cache prefix) is left untouched. Used by
/// the headless (`--no-default-features`) build; `tui` uses [`history_doc`].
#[cfg(not(feature = "tui"))]
fn render_transcript(session: &Session) -> String {
    let mut out = String::new();
    for m in session.messages() {
        match m.role.as_str() {
            "system" => continue, // the system prompt isn't part of the visible chat
            "user" => out.push_str(&format!("▌ 你\n{}\n\n", m.content.trim_end())),
            "assistant" => {
                for tc in &m.tool_calls {
                    out.push_str(&format!(
                        "▌ OrcaRein → {}({})\n\n",
                        tc.function.name, tc.function.arguments
                    ));
                }
                if !m.content.trim().is_empty() {
                    out.push_str(&format!("▌ OrcaRein\n{}\n\n", m.content.trim_end()));
                }
            }
            "tool" => out.push_str(&format!("▌ 工具结果\n{}\n\n", m.content.trim_end())),
            other => out.push_str(&format!("▌ {other}\n{}\n\n", m.content.trim_end())),
        }
    }
    if out.is_empty() {
        out.push_str("(对话为空)\n");
    }
    out
}

/// Normalize a `/show` path argument: trim, and strip a leading `@` left by the
/// mention popup's autocompletion, so `@path` and `path` both show `path`.
fn show_arg_path(arg: &str) -> &str {
    let a = arg.trim();
    a.strip_prefix('@').unwrap_or(a)
}

/// Map a (lowercased) path's extension to a `syntax` language name, so `/show`
/// can syntax-highlight a standalone code file. `None` → not a known code file.
fn lang_for_ext(lower: &str) -> Option<&'static str> {
    let (_, ext) = lower.rsplit_once('.')?;
    Some(match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "js",
        "ts" => "ts",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "sh" | "bash" => "bash",
        "json" => "json",
        "toml" => "toml",
        _ => return None,
    })
}

/// `/show <path>`: reads a file and shows it through the pager. Read failures
/// are reported, not fatal.
fn run_show(path: &str) {
    let path = show_arg_path(path);
    if path.is_empty() {
        eprintln!("用法：/show <文件路径>");
        return;
    }
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let lower = path.to_lowercase();
            let kind = if lower.ends_with(".md") || lower.ends_with(".markdown") {
                overlay::DocKind::Markdown
            } else if let Some(lang) = lang_for_ext(&lower) {
                overlay::DocKind::Code(lang.to_string())
            } else {
                overlay::DocKind::Plain
            };
            if let Err(e) = overlay::show_paged(path, &content, kind) {
                eprintln!("显示失败：{e}");
            }
        }
        Err(e) => eprintln!("读不了 {path}：{e}"),
    }
}

/// Formats the registry's tool names for the `/tools` command, splitting
/// built-in tools from MCP-provided ones (`mcp__<server>__<tool>`) so the user
/// can confirm at a glance which MCP servers loaded.
fn format_tool_list(names: &[&str]) -> String {
    let mcp: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| n.starts_with("mcp__"))
        .collect();
    let builtin: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !n.starts_with("mcp__"))
        .collect();
    let fmt = |v: &[&str]| {
        if v.is_empty() {
            "（无）".to_string()
        } else {
            v.join(", ")
        }
    };
    format!(
        "工具（{}）：\n  内置（{}）：{}\n  MCP（{}）：{}",
        names.len(),
        builtin.len(),
        fmt(&builtin),
        mcp.len(),
        fmt(&mcp),
    )
}

/// Color token for a context-fill percentage: `>=80%` error, `>=50%` warning,
/// otherwise accent. Pure — unit-tested.
fn ctx_token(pct: f64) -> color::Token {
    if pct >= 80.0 {
        color::Token::Error
    } else if pct >= 50.0 {
        color::Token::Warning
    } else {
        color::Token::Accent
    }
}

/// Expand short model aliases for the active provider: under `deepseek`,
/// `pro` → `deepseek-v4-pro` and `flash` → `deepseek-v4-flash`; anything else
/// (and other providers) is returned unchanged. Pure — unit-tested.
fn expand_model_alias(name: &str, provider: &str) -> String {
    if provider == "deepseek" {
        match name {
            "pro" => return "deepseek-v4-pro".to_string(),
            "flash" => return "deepseek-v4-flash".to_string(),
            _ => {}
        }
    }
    name.to_string()
}

/// Built-in model list when a live `/v1/models` fetch fails: the two deepseek V4
/// ids, else just the current model so `/model` can still echo it. Pure.
fn fallback_models(provider: &str, current: &str) -> Vec<String> {
    match provider {
        "deepseek" => vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ],
        _ => vec![current.to_string()],
    }
}

/// Guarantee `current` is in `choices` (so validation never rejects the running
/// model), then sort + dedup for a stable popup order. Pure.
fn ensure_current_present(choices: &mut Vec<String>, current: &str) {
    if !choices.iter().any(|m| m == current) {
        choices.push(current.to_string());
    }
    choices.sort();
    choices.dedup();
}

/// Whether `model` is one of the resolved `choices` (the live `/v1/models` list,
/// or the fallback when that fetch failed). Generalizes the Phase 1 hard-coded
/// deepseek check so `/model` accepts any catalogued model and rejects typos.
fn is_known_model(model: &str, choices: &[String]) -> bool {
    choices.iter().any(|m| m == model)
}

/// The compact model label for the status bar: strip the `deepseek-v4-` prefix
/// (`deepseek-v4-pro` → `pro`), otherwise the id unchanged. The display inverse
/// of [`expand_model_alias`]. Pure — unit-tested. Only the modal status bar (tui)
/// consumes it, so it is `tui`-gated to stay dead-code-free under
/// `--no-default-features`.
#[cfg(feature = "tui")]
fn short_model(model: &str) -> &str {
    model.strip_prefix("deepseek-v4-").unwrap_or(model)
}

/// Persist the chosen `model` to the config file at `path` (the global
/// `config.toml`), so the next session's [`resolve`] picks it up and the header
/// shows it. Loads the existing config (missing = default), sets `model`, writes
/// it back. Errors (e.g. a read-only config dir) bubble up for the caller to warn.
fn persist_model_choice(path: &std::path::Path, model: &str) -> anyhow::Result<()> {
    let mut config = Config::load_from(path)?;
    config.set("model", model)?;
    config.save_to(path)?;
    Ok(())
}

/// The context-occupancy line (`ctx 2.4% (24k/1.0M)`), colored by threshold.
/// `None` when the model's window is unknown (caller omits / falls back).
fn ctx_colored(prompt_tokens: u64, model: &str, mode: color::ColorMode) -> Option<String> {
    let window = cost::context_window(model)?;
    let line = cost::context_line(prompt_tokens, model)?;
    let pct = if window > 0 {
        prompt_tokens as f64 / window as f64 * 100.0
    } else {
        0.0
    };
    Some(color::paint(mode, ctx_token(pct), &line))
}

/// Renders the `/help` command list as a two-column block (REPL-only, so always
/// interactive; color degrades via `mode`). Pure — the column alignment is
/// unit-tested. Columns align on a fixed left-block width; CJK 说明 counts as 2
/// display columns per char.
fn render_help(mode: color::ColorMode) -> String {
    use color::Token;
    use header::disp_width;
    let p = |t: Token, s: &str| color::paint(mode, t, s);

    // (command, 说明): left column then right column, row-major. An empty right
    // command ("","") renders left-only.
    const ROWS: &[[(&str, &str); 2]] = &[
        [("/help", "显示帮助"), ("/init", "生成 AGENTS.md")],
        [("/clear", "清空对话"), ("/compact", "压缩上下文")],
        [("/save", "保存会话"), ("/usage", "用量与花费")],
        [("/tools", "列出工具"), ("/show", "查看文件（分页）")],
        [("/history", "浏览记录"), ("/model", "切换模型")],
        [("/sessions", "列出会话"), ("/resume", "切换会话")],
        [("/new", "新建会话"), ("/skills", "列出可用 skill")],
        [("/exit", "退出会话"), ("/orca", "召唤鲸鱼")],
    ];
    const LCMD: usize = 11; // left command field width
    const LBLOCK: usize = 34; // left block width (leading 2 + cmd + 说明 + fill)
    const RCMD: usize = 10; // right command field width

    let mut out = format!(
        "{}{}  {}\n",
        p(Token::Brand, "▌ "),
        p(Token::OrcaWhite, "commands"),
        p(Token::Dim, "命令一览"),
    );
    out.push_str(&format!("  {}\n", p(Token::Dim, &"─".repeat(46))));
    for row in ROWS {
        let (lc, ld) = row[0];
        let (rc, rd) = row[1];
        let lcmd_fill = LCMD.saturating_sub(disp_width(lc));
        if rc.is_empty() {
            // Left-only row.
            out.push_str(&format!(
                "  {}{}{}\n",
                p(Token::Accent, lc),
                " ".repeat(lcmd_fill),
                p(Token::Dim, ld),
            ));
            continue;
        }
        let mid_fill = LBLOCK.saturating_sub(2 + LCMD + disp_width(ld));
        let rcmd_fill = RCMD.saturating_sub(disp_width(rc));
        out.push_str(&format!(
            "  {}{}{}{}{}{}{}\n",
            p(Token::Accent, lc),
            " ".repeat(lcmd_fill),
            p(Token::Dim, ld),
            " ".repeat(mid_fill),
            p(Token::Accent, rc),
            " ".repeat(rcmd_fill),
            p(Token::Dim, rd),
        ));
    }
    out.trim_end().to_string()
}

/// Handles a slash command (the leading `/` already stripped). Takes the
/// session store + active id so `/save` can persist on demand, the model
/// and last turn's prompt-token count so `/usage` can show context fill + cost,
/// and the registered tool names for `/tools`.
#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: &str,
    session: &mut Session,
    store: &SessionStore,
    session_id: &str,
    created_at_ms: u64,
    model: &str,
    last_prompt_tokens: u64,
    tool_names: &[&str],
    skills: &[orcarein_core::Skill],
    mode: color::ColorMode,
) -> CommandAction {
    // Split into a verb and an optional argument so `/show <path>` works while
    // bare verbs (`/clear`) still match.
    let (verb, arg) = match cmd.split_once(char::is_whitespace) {
        Some((v, a)) => (v, a.trim()),
        None => (cmd, ""),
    };
    match verb {
        "exit" | "quit" => CommandAction::Quit,
        "clear" => {
            session.clear();
            println!("(会话已清空，system prompt 保留)");
            CommandAction::Continue
        }
        "save" => {
            match store.save(session_id, created_at_ms, session) {
                Ok(()) => println!("已保存：{}", store.path_for(session_id).display()),
                Err(e) => eprintln!("保存失败：{e}"),
            }
            CommandAction::Continue
        }
        "usage" | "context" => {
            let u = session.usage();
            println!(
                "[累计 tokens: prompt {} / completion {} / total {}; 当前 {} 轮]",
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                session.turn_count()
            );
            // Context fill (last turn's prompt ≈ current window use) + cost,
            // when the model is known. ctx colored by threshold (≥50% warn / ≥80% err).
            match ctx_colored(last_prompt_tokens, model, mode) {
                Some(c) => println!("[{c}]"),
                None => println!("[ctx: 模型 {model} 的上下文窗口未知]"),
            }
            if let Some(meter) = cost::meter_line(&u, model) {
                println!("[{meter}]");
            }
            CommandAction::Continue
        }
        // `/show` and `/history` render through the pager. They are pure
        // presentation — neither pushes anything into the session.
        "tools" => {
            println!("{}", format_tool_list(tool_names));
            CommandAction::Continue
        }
        "skills" => {
            if skills.is_empty() {
                println!("没有发现 skill（在 .orcarein/skills/ 放 *.md 或 <name>/SKILL.md）。");
            } else {
                print!("{}", orcarein_core::skills_list(skills));
            }
            CommandAction::Continue
        }
        "show" => {
            run_show(arg);
            CommandAction::Continue
        }
        "init" => CommandAction::RunInit,
        "compact" => CommandAction::RunCompact,
        "model" => {
            if arg.is_empty() {
                println!("当前 model：{model}");
                println!("用法：/model <name>（deepseek 可用简写 flash / pro）");
                CommandAction::Continue
            } else {
                CommandAction::SwitchModel(arg.to_string())
            }
        }
        "sessions" => {
            if let Err(e) = run_session_list() {
                eprintln!("列出 session 失败：{e}");
            }
            CommandAction::Continue
        }
        "resume" => {
            if arg.is_empty() {
                eprintln!("用法：/resume <session id 或前缀>（用 /sessions 看列表）");
                CommandAction::Continue
            } else {
                CommandAction::SwitchSession(arg.to_string())
            }
        }
        "new" => CommandAction::NewSession,
        "orca" => CommandAction::Swim,
        "history" => {
            #[cfg(feature = "tui")]
            {
                let doc = history_doc(session, overlay::term_cols(), mode);
                if let Err(e) = overlay::show_doc("对话记录", doc) {
                    eprintln!("显示失败：{e}");
                }
            }
            #[cfg(not(feature = "tui"))]
            {
                let transcript = render_transcript(session);
                if let Err(e) =
                    overlay::show_paged("对话记录", &transcript, overlay::DocKind::Plain)
                {
                    eprintln!("显示失败：{e}");
                }
            }
            CommandAction::Continue
        }
        "help" => {
            println!("{}", render_help(mode));
            CommandAction::Continue
        }
        other => {
            eprintln!("未知命令：/{other}。/help 查看支持的命令。");
            CommandAction::Continue
        }
    }
}

/// Tool-iteration cap for `issue` mode — higher than the default so the agent
/// can explore and edit several files in one turn.
const ISSUE_MAX_ITERATIONS: usize = 25;

/// Event sink for `issue` mode: logs the agent's tool activity to stderr so the
/// operator can watch what it's doing. Reasoning/content stay quiet.
struct IssueSink;

impl EventSink for IssueSink {
    fn emit(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ToolStarted {
                name, arguments, ..
            } => eprintln!("orcarein: [tool] {name}({arguments})"),
            AgentEvent::ToolFinished {
                name,
                is_error,
                result,
                ..
            } => {
                if is_error {
                    eprintln!("orcarein: [tool] {name} -> {result}");
                } else {
                    eprintln!("orcarein: [tool] {name} -> {} bytes", result.len());
                }
            }
            AgentEvent::IterationLimit => eprintln!("orcarein: [iteration limit]"),
            _ => {}
        }
    }
}

/// Runs `git` with `args`, returning trimmed stdout. Errors (with stderr) on a
/// non-zero exit.
fn git(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .context("failed to run git (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Best-effort `cargo test`: `Some(true/false)` if this is a Cargo project,
/// `None` if there's no `Cargo.toml` to test.
fn run_cargo_tests() -> Option<bool> {
    if !std::path::Path::new("Cargo.toml").exists() {
        return None;
    }
    eprintln!("orcarein: running `cargo test` to verify…");
    let status = std::process::Command::new("cargo")
        .args(["test", "--quiet"])
        .status()
        .ok()?;
    Some(status.success())
}

/// `orcarein issue <n>` — the BYO-key self-bootstrap loop. Returns a process
/// exit code.
async fn run_issue(cli: &Cli, number: u64) -> i32 {
    match run_issue_inner(cli, number).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("orcarein: {e:#}");
            1
        }
    }
}

async fn run_issue_inner(cli: &Cli, number: u64) -> Result<i32> {
    // 1. Must be inside a CLEAN git repo, so the resulting diff is purely the
    //    agent's work.
    if git(&["rev-parse", "--is-inside-work-tree"]).unwrap_or_default() != "true" {
        bail!("not inside a git repository — run `orcarein issue` from a clone");
    }
    if !git(&["status", "--porcelain"])?.is_empty() {
        bail!("working tree is not clean — commit or stash first so the diff is purely the agent's work");
    }

    // 2. Identify the repo from the origin remote.
    let remote = git(&["remote", "get-url", "origin"]).context("no `origin` remote found")?;
    let (owner, repo) = parse_owner_repo(&remote)?;
    eprintln!("orcarein: {owner}/{repo}, fixing issue #{number}");

    // 3. Fetch the issue (token optional for public repos).
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());
    let issue = fetch_issue(&owner, &repo, number, token.as_deref())
        .await
        .context("failed to fetch the issue from GitHub")?;
    eprintln!("orcarein: issue: {}", issue.title);

    // 4. Resolve the model/provider (DeepSeek by default, BYO key).
    let Resolved {
        provider,
        model,
        hooks,
        ..
    } = resolve(cli)?;

    // 5. Restricted toolset: read/list/search/edit/write only — NO shell.
    let issue_tools: Vec<String> = ["read_file", "list_dir", "search", "edit", "write_file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut registry = build_registry(Some(&issue_tools));

    let skills =
        orcarein_core::discover_skills(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let skills_index = if skills.is_empty() {
        None
    } else {
        Some(orcarein_core::skills_index(&skills))
    };

    // Register the `task` subagent tool. Children mirror the issue path's
    // restricted posture: the same edit-only allowlist (no shell), backed by a
    // child registry built from `issue_tools`. Clone the provider Arc into the
    // tool before `Agent::new` borrows `&*provider`.
    {
        let child_tools = issue_tools.clone();
        let policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> =
            Arc::new(|| {
                Box::new(AllowlistPolicy::from_allowed([
                    "list_dir",
                    "search",
                    "edit",
                    "write_file",
                ]))
            });
        register_subagent(
            &mut registry,
            Arc::clone(&provider),
            Some(child_tools),
            model.clone(),
            policy_factory,
            // Default placeholders (issue mode has no `permission_rules`
            // either) — wired to the child properly in a later cut.
            Arc::new(Ruleset::with_defaults),
            SharedMode::new(PermissionMode::Default),
            hooks.clone(),
        );
    }
    if !skills.is_empty() {
        registry.register(Box::new(SkillTool::new(skills)));
    }
    let tool_defs = registry.definitions();
    let agent = Agent::new(provider.as_ref(), &registry, &tool_defs)
        .with_cache_mode(cache_mode(cli))
        .with_max_iterations(ISSUE_MAX_ITERATIONS)
        .with_hooks(hooks);

    // 6. Let the agent work the issue. The Risky tools it may use are gated to
    //    exactly the edit set; read_file is Safe and always allowed.
    let mut policy: Box<dyn PermissionPolicy> = Box::new(AllowlistPolicy::from_allowed([
        "list_dir",
        "search",
        "edit",
        "write_file",
    ]));
    let system = format!(
        "You are OrcaRein, working as an autonomous maintainer of the repository {owner}/{repo}. \
         Fix GitHub issue #{number}. Explore the codebase with search, read_file, and list_dir, \
         then make minimal, focused changes with edit and write_file. Do NOT run shell commands. \
         The working directory is the repository root. When you are done, briefly summarize what \
         you changed."
    );
    // The issue fix benefits most from project context — inject AGENTS.md here too.
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let system = append_skills_index(fresh_session_prompt(system, &cwd), skills_index.as_deref());
    let mut session = Session::new(system);
    session.push_user(format!(
        "Issue #{number}: {}\n\n{}",
        issue.title, issue.body
    ));

    let mut sink = IssueSink;
    let outcome = agent
        .run_turn(&mut session, &model, policy.as_mut(), &mut sink)
        .await
        .context("the agent run failed")?;

    eprintln!(
        "\norcarein: --- agent summary ---\n{}",
        outcome.content.trim()
    );
    if outcome.hit_iteration_limit {
        eprintln!(
            "orcarein: warning: hit the tool-iteration limit ({ISSUE_MAX_ITERATIONS}); changes may be incomplete"
        );
    }
    if let Some(m) = cost::meter_line(&outcome.usage, &model) {
        eprintln!("orcarein: [{m}]");
    }

    // 7. Did it change anything?
    let diff = git(&["diff"])?;
    if diff.is_empty() {
        eprintln!("orcarein: the agent made no file changes.");
        return Ok(0);
    }

    // 8. Verify (best-effort) — the HARNESS runs tests, not the model.
    let tests = run_cargo_tests();

    // 9. Show the diff and stop. Committing / pushing / opening the PR is your
    //    call (E1 keeps the human in the loop).
    println!("{diff}");
    eprintln!(
        "\norcarein: review the diff above. Tests: {}.",
        match tests {
            Some(true) => "passed",
            Some(false) => "FAILED",
            None => "skipped (no Cargo.toml)",
        }
    );
    eprintln!(
        "orcarein: to ship it — git switch -c fix-issue-{number} && git commit -am \"fix: …\" \
         && git push -u origin HEAD, then open a PR with \"Closes #{number}\"."
    );

    Ok(if tests == Some(false) { 2 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use orcarein_core::Message;

    #[test]
    fn build_provider_without_a_key_names_login_and_the_env_var() {
        // P5: the headless (`run` / `issue`) no-key error is a contract. It had
        // zero coverage — "the existing tests still pass" proved nothing about it.
        let err = super::build_provider("deepseek", None, RetryPolicy::from_config(Some(0)))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("no API key"), "{err}");
        assert!(err.contains("orcarein login"), "{err}");
        assert!(err.contains("DEEPSEEK_API_KEY"), "{err}");
    }

    #[test]
    fn build_provider_rejects_an_unknown_provider_before_asking_for_a_key() {
        // Order matters: a typo'd provider must not be masked by a missing key.
        let err = super::build_provider("mystery", None, RetryPolicy::from_config(Some(0)))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("unknown provider"), "{err}");
    }

    #[test]
    fn validate_provider_accepts_known_and_rejects_unknown() {
        assert!(super::validate_provider("deepseek").is_ok());
        assert!(super::validate_provider("openai").is_ok());
        let err = super::validate_provider("anthropic")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown provider"), "{err}");
    }

    #[test]
    fn interactive_requires_both_ends_to_be_a_tty() {
        // `&&`, not `||`. Getting this wrong is P1: with stdout a tty and stdin a
        // file (`orcarein < prompts.txt`), a `||` would send us down the prompt
        // path, `read_secret_line` would read the FIRST LINE OF THE SCRIPT as the
        // API key, and an offline verify (Inconclusive) would save it to
        // secrets.toml.
        assert!(super::interactive(true, true));
        assert!(!super::interactive(false, true));
        assert!(!super::interactive(true, false));
        assert!(!super::interactive(false, false));
    }

    #[test]
    fn first_run_decision_prompts_only_when_interactive_and_keyless() {
        use super::FirstRun;
        assert_eq!(
            super::first_run_decision(Some("sk-abc"), true),
            FirstRun::Proceed
        );
        assert_eq!(
            super::first_run_decision(Some("sk-abc"), false),
            FirstRun::Proceed
        );
        assert_eq!(super::first_run_decision(None, true), FirstRun::Prompt);
        assert_eq!(super::first_run_decision(None, false), FirstRun::Bail);
        // A blank key is no key (mirrors SecretStore::resolve's trim).
        assert_eq!(
            super::first_run_decision(Some("   "), true),
            FirstRun::Prompt
        );
        assert_eq!(
            super::first_run_decision(Some("   "), false),
            FirstRun::Bail
        );
    }

    #[test]
    fn signup_url_maps_known_providers() {
        assert_eq!(
            super::signup_url("deepseek"),
            Some("https://platform.deepseek.com/api_keys")
        );
        assert_eq!(
            super::signup_url("openai"),
            Some("https://platform.openai.com/api-keys")
        );
        assert_eq!(super::signup_url("mystery"), None);
    }

    #[test]
    fn project_memory_block_appends_when_present_and_skips_when_absent() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();

        // Absent -> None.
        assert!(super::project_memory_block(dir.path()).is_none());

        // Present -> Some block carrying the delimiter + content.
        std::fs::write(dir.path().join("AGENTS.md"), "use cargo test").unwrap();
        let block = super::project_memory_block(dir.path()).expect("present -> Some");
        assert!(block.contains("# Project context"));
        assert!(block.contains("use cargo test"));
    }

    #[test]
    fn render_help_lists_all_commands_and_aligns_columns() {
        // Plain (no color) so we can measure display columns directly.
        let out = super::render_help(crate::color::ColorMode::None);
        for cmd in [
            "/help",
            "/clear",
            "/save",
            "/tools",
            "/history",
            "/init",
            "/compact",
            "/usage",
            "/show",
            "/exit",
            "/model",
            "/sessions",
            "/resume",
            "/new",
            "/skills",
            "/orca",
        ] {
            assert!(out.contains(cmd), "help missing {cmd}");
        }
        // Every data row's right-hand command starts at the same display column.
        for (line, rcmd) in out
            .lines()
            .skip(2) // title + rule
            .zip([
                "/init", "/compact", "/usage", "/show", "/model", "/resume", "/skills", "/orca",
            ])
        {
            let pos = line.find(rcmd).expect("right command present");
            assert_eq!(
                crate::header::disp_width(&line[..pos]),
                34,
                "right column misaligned in {line:?}"
            );
        }
    }

    #[test]
    fn ctx_token_thresholds() {
        use crate::color::Token;
        assert_eq!(super::ctx_token(0.0), Token::Accent);
        assert_eq!(super::ctx_token(49.9), Token::Accent);
        assert_eq!(super::ctx_token(50.0), Token::Warning);
        assert_eq!(super::ctx_token(79.9), Token::Warning);
        assert_eq!(super::ctx_token(80.0), Token::Error);
        assert_eq!(super::ctx_token(100.0), Token::Error);
    }

    #[test]
    fn show_arg_path_strips_mention_at_and_trims() {
        // Mention popup autocompletes `@path`; /show must read `path`.
        assert_eq!(super::show_arg_path("@crates/a.rs"), "crates/a.rs");
        assert_eq!(super::show_arg_path("  @crates/a.rs  "), "crates/a.rs");
        // Plain path unchanged.
        assert_eq!(super::show_arg_path("crates/a.rs"), "crates/a.rs");
        // Only the leading @ is stripped.
        assert_eq!(super::show_arg_path("a@b.rs"), "a@b.rs");
    }

    #[test]
    fn lang_for_ext_maps_known_code_extensions() {
        assert_eq!(super::lang_for_ext("foo/bar.rs"), Some("rust"));
        assert_eq!(super::lang_for_ext("a.py"), Some("python"));
        assert_eq!(super::lang_for_ext("a.cpp"), Some("cpp"));
        assert_eq!(super::lang_for_ext("a.json"), Some("json"));
        // Non-code / no extension → None (falls back to plain).
        assert_eq!(super::lang_for_ext("readme.txt"), None);
        assert_eq!(super::lang_for_ext("noext"), None);
    }

    #[test]
    fn expand_model_alias_only_for_deepseek() {
        assert_eq!(
            super::expand_model_alias("pro", "deepseek"),
            "deepseek-v4-pro"
        );
        assert_eq!(
            super::expand_model_alias("flash", "deepseek"),
            "deepseek-v4-flash"
        );
        // Full names and other providers pass through unchanged.
        assert_eq!(
            super::expand_model_alias("deepseek-v4-pro", "deepseek"),
            "deepseek-v4-pro"
        );
        assert_eq!(super::expand_model_alias("pro", "openai"), "pro");
    }

    #[test]
    fn fallback_models_lists_known_deepseek_else_current() {
        assert_eq!(
            super::fallback_models("deepseek", "deepseek-v4-flash"),
            vec![
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string()
            ]
        );
        assert_eq!(
            super::fallback_models("openai", "gpt-4o"),
            vec!["gpt-4o".to_string()]
        );
    }

    #[test]
    fn ensure_current_present_inserts_and_sorts() {
        let mut v = vec!["b".to_string(), "a".to_string()];
        super::ensure_current_present(&mut v, "c");
        assert_eq!(v, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        // idempotent when already present
        super::ensure_current_present(&mut v, "a");
        assert_eq!(v, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn is_known_model_checks_membership() {
        let choices = vec![
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ];
        assert!(super::is_known_model("deepseek-v4-pro", &choices));
        // Typo / nonexistent model rejected (the reported bug: `flas`).
        assert!(!super::is_known_model("flas", &choices));
        assert!(!super::is_known_model("deepseek-chat", &choices));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn short_model_strips_deepseek_prefix() {
        // The compact label shown on the status bar (inverse of expand_model_alias).
        assert_eq!(super::short_model("deepseek-v4-pro"), "pro");
        assert_eq!(super::short_model("deepseek-v4-flash"), "flash");
        // A future model id keeps only its tail.
        assert_eq!(super::short_model("deepseek-v4-4.1"), "4.1");
        // Other providers' ids pass through untouched.
        assert_eq!(super::short_model("gpt-4o"), "gpt-4o");
        assert_eq!(super::short_model("mock"), "mock");
    }

    #[test]
    fn persist_model_choice_writes_and_reloads() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Persisting into a missing config creates it with the model set.
        super::persist_model_choice(&path, "deepseek-v4-pro").unwrap();
        let cfg = orcarein_core::Config::load_from(&path).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("deepseek-v4-pro"));
        // A later switch overwrites the persisted choice.
        super::persist_model_choice(&path, "deepseek-v4-flash").unwrap();
        let cfg2 = orcarein_core::Config::load_from(&path).unwrap();
        assert_eq!(cfg2.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn fresh_session_prompt_injects_exactly_one_block() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "facts").unwrap();

        let base = "You are OrcaRein.".to_string();
        let prompt = super::fresh_session_prompt(base.clone(), dir.path());
        assert!(prompt.starts_with(&base), "base persona must be preserved");
        assert_eq!(
            prompt.matches("# Project context").count(),
            1,
            "fresh session injects exactly one block"
        );
    }

    #[test]
    fn append_skills_index_adds_block_when_present() {
        let out = super::append_skills_index("BASE".to_string(), Some("# Available skills"));
        assert_eq!(out, "BASE\n\n# Available skills");
    }

    #[test]
    fn append_skills_index_is_noop_when_absent() {
        assert_eq!(super::append_skills_index("BASE".to_string(), None), "BASE");
    }

    #[test]
    fn resumed_session_prompt_is_used_verbatim_no_reinjection() {
        // Spec §6 #9: resume keeps the frozen prompt. A session whose prompt was
        // baked earlier (already carries one block) must not gain a second block
        // when resumed. The resume branch reuses the loaded Session's prompt as-is
        // and never calls `fresh_session_prompt`; this guards that invariant.
        let baked = "You are OrcaRein.\n\n# Project context (from AGENTS.md)\n\nfacts\n";
        let session = Session::new(baked);
        // The system prompt is messages()[0] (a system Message); resume reuses it verbatim.
        let system = &session.messages()[0].content;
        assert_eq!(system.matches("# Project context").count(), 1);
    }

    #[tokio::test]
    async fn handle_compact_shrinks_session_and_returns_true() {
        use orcarein_core::{Message, MockProvider, Session, StreamEvent, TokenUsage};
        let mut session = Session::new("SYS");
        for i in 0..5 {
            session.push_user(format!("u{i}"));
            session.push_assistant(Message::assistant(format!("a{i}")));
        }
        let before = session.messages().len();
        let provider = MockProvider::new();
        provider.push_response(vec![
            StreamEvent::Content("summary".into()),
            StreamEvent::Usage(TokenUsage {
                total_tokens: 5,
                ..Default::default()
            }),
        ]);

        let compacted = super::handle_compact(&provider, "mock-model", &mut session).await;
        assert!(compacted);
        assert!(session.messages().len() < before);
    }

    #[tokio::test]
    async fn handle_compact_nothing_to_do_returns_false() {
        use orcarein_core::{MockProvider, Session};
        let mut session = Session::new("SYS");
        session.push_user("only one");
        let provider = MockProvider::new();
        let compacted = super::handle_compact(&provider, "mock-model", &mut session).await;
        assert!(!compacted);
    }

    #[tokio::test]
    async fn handle_init_runs_a_turn_that_writes_agents_md() {
        use orcarein_core::MockProvider;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");

        // Script the model: one tool call writing AGENTS.md, then a final summary.
        let provider = MockProvider::new();
        let args = format!(
            r##"{{"path":"{}","content":"# Project\nDemo."}}"##,
            target.display().to_string().replace('\\', "\\\\")
        );
        provider.push_tool_call("c1", "write_file", &args);
        provider.push_text("Wrote AGENTS.md.");

        super::handle_init(&provider, "mock-model", dir.path(), &HookSet::empty()).await;

        assert!(
            target.is_file(),
            "handle_init should have written AGENTS.md"
        );
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("# Project"));
    }

    #[test]
    fn init_precondition_classifies_cwd_parent_and_none() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let sub = dir.path().join("child");
        std::fs::create_dir_all(&sub).unwrap();

        // Nothing anywhere -> Proceed.
        assert!(matches!(
            super::init_precondition(&sub),
            super::InitDecision::Proceed
        ));

        // Parent has one, cwd doesn't -> ProceedShadowing(parent file).
        std::fs::write(dir.path().join("AGENTS.md"), "root").unwrap();
        match super::init_precondition(&sub) {
            super::InitDecision::ProceedShadowing(p) => {
                assert_eq!(p, dir.path().join("AGENTS.md"))
            }
            other => panic!("expected ProceedShadowing, got {other:?}"),
        }

        // cwd has one -> Exists.
        std::fs::write(sub.join("AGENTS.md"), "child").unwrap();
        assert!(matches!(
            super::init_precondition(&sub),
            super::InitDecision::Exists
        ));
    }

    #[test]
    fn clap_definition_is_valid() {
        // Catches derive-level mistakes (e.g. a positional clashing with a
        // subcommand) at test time instead of first run.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_args_is_repl() {
        let cli = Cli::try_parse_from(["orcarein"]).unwrap();
        assert!(cli.model.is_none());
        assert!(cli.command.is_none());
        assert!(!cli.no_permission);
        assert!(!cli.no_economy);
    }

    #[test]
    fn permission_mode_flag_parses() {
        let cli = Cli::try_parse_from(["orcarein", "--permission-mode", "plan"]).unwrap();
        assert_eq!(cli.permission_mode, Some(PermissionMode::Plan));
    }

    #[test]
    fn permission_mode_flag_defaults_none() {
        let cli = Cli::try_parse_from(["orcarein"]).unwrap();
        assert_eq!(cli.permission_mode, None);
    }

    #[test]
    fn no_economy_flag_parses() {
        let cli = Cli::try_parse_from(["orcarein", "--no-economy"]).unwrap();
        assert!(cli.no_economy);
        assert_eq!(cache_mode(&cli), CacheMode::Benchmark);
        let cli = Cli::try_parse_from(["orcarein"]).unwrap();
        assert_eq!(cache_mode(&cli), CacheMode::Economy);
    }

    #[test]
    fn positional_model_parses() {
        let cli = Cli::try_parse_from(["orcarein", "deepseek-v4-pro"]).unwrap();
        assert_eq!(cli.model.as_deref(), Some("deepseek-v4-pro"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn provider_and_csv_tools_parse() {
        let cli = Cli::try_parse_from([
            "orcarein",
            "--provider",
            "openai",
            "--tools",
            "read_file,list_dir",
        ])
        .unwrap();
        assert_eq!(cli.provider.as_deref(), Some("openai"));
        assert_eq!(
            cli.tools.as_deref(),
            Some(&["read_file".to_owned(), "list_dir".to_owned()][..])
        );
    }

    #[test]
    fn config_set_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "config", "set", "provider", "openai"]).unwrap();
        match cli.command {
            Some(Command::Config {
                action: ConfigAction::Set { key, value },
            }) => {
                assert_eq!(key, "provider");
                assert_eq!(value, "openai");
            }
            other => panic!("expected config set, got {other:?}"),
        }
        // A subcommand must not be mistaken for the positional model.
        assert!(cli.model.is_none());
    }

    #[test]
    fn config_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "config", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::List
            })
        ));
    }

    #[test]
    fn session_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "session", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                action: SessionAction::List
            })
        ));
    }

    #[test]
    fn session_resume_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "session", "resume", "1748789422123"]).unwrap();
        match cli.command {
            Some(Command::Session {
                action: SessionAction::Resume { id },
            }) => assert_eq!(id.as_deref(), Some("1748789422123")),
            other => panic!("expected session resume, got {other:?}"),
        }
        assert!(cli.model.is_none());
    }

    #[test]
    fn session_delete_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "session", "delete", "1748789422123"]).unwrap();
        match cli.command {
            Some(Command::Session {
                action: SessionAction::Delete { id },
            }) => assert_eq!(id, "1748789422123"),
            other => panic!("expected session delete, got {other:?}"),
        }
        assert!(cli.model.is_none());
    }

    #[test]
    fn session_resume_without_id_parses_as_none() {
        // `session resume` with no positional triggers the interactive picker;
        // clap must accept the missing id as `None`, not error.
        let cli = Cli::try_parse_from(["orcarein", "session", "resume"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                action: SessionAction::Resume { id: None }
            })
        ));
    }

    fn summaries(ids: &[&str]) -> Vec<SessionSummary> {
        ids.iter()
            .map(|id| SessionSummary {
                id: (*id).to_owned(),
                created_at_ms: 0,
                turns: 0,
                title: String::new(),
            })
            .collect()
    }

    #[test]
    fn decide_delete_unique_prefix_targets_one() {
        let s = summaries(&["1748111", "1799222"]);
        assert_eq!(
            decide_delete("1748", &s, |_| false),
            DeleteAction::Delete("1748111".to_owned())
        );
    }

    #[test]
    fn decide_delete_ambiguous_refuses() {
        let s = summaries(&["1748111", "1748222"]);
        match decide_delete("1748", &s, |_| false) {
            DeleteAction::Ambiguous(hits) => assert_eq!(hits.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn decide_delete_no_match_deletes_nothing() {
        // Regression guard: a non-matching needle must NEVER resolve to a
        // present session (the "delete 99999 wiped 1780…" scare).
        let s = summaries(&["1780737346422"]);
        assert_eq!(
            decide_delete("99999", &s, |_| false),
            DeleteAction::NotFound
        );
    }

    #[test]
    fn decide_delete_exact_corrupt_file_falls_back() {
        // id absent from the parseable summaries, but its file exists on disk.
        let s = summaries(&[]);
        assert_eq!(
            decide_delete("123", &s, |e| e == "123"),
            DeleteAction::Delete("123".to_owned())
        );
    }

    #[test]
    fn decide_delete_blank_is_not_found_even_if_file_exists() {
        let s = summaries(&["1780737346422"]);
        assert_eq!(decide_delete("", &s, |_| true), DeleteAction::NotFound);
    }

    #[test]
    fn resolve_id_prefix_exact_full_id_wins() {
        // An exact id match wins even when it is also a prefix of a longer id.
        let ids = vec!["1748".to_owned(), "1748999".to_owned()];
        assert_eq!(
            resolve_id_prefix("1748", &ids),
            IdMatch::One("1748".to_owned())
        );
    }

    #[test]
    fn resolve_id_prefix_unique_prefix_resolves() {
        let ids = vec!["1748111".to_owned(), "1799222".to_owned()];
        assert_eq!(
            resolve_id_prefix("1748", &ids),
            IdMatch::One("1748111".to_owned())
        );
        // leading/trailing whitespace is trimmed
        assert_eq!(
            resolve_id_prefix(" 1799 ", &ids),
            IdMatch::One("1799222".to_owned())
        );
    }

    #[test]
    fn resolve_id_prefix_none_when_nothing_matches_or_empty() {
        let ids = vec!["1748111".to_owned(), "1748222".to_owned()];
        assert_eq!(resolve_id_prefix("9", &ids), IdMatch::None);
        // an empty needle matches nothing (safe: never deletes/resumes blindly)
        assert_eq!(resolve_id_prefix("", &ids), IdMatch::None);
    }

    #[test]
    fn resolve_id_prefix_ambiguous_lists_all_hits() {
        let ids = vec!["1748111".to_owned(), "1748222".to_owned()];
        match resolve_id_prefix("1748", &ids) {
            IdMatch::Many(hits) => assert_eq!(hits.len(), 2),
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn resolve_pick_maps_1based_choice_to_id() {
        let s = summaries(&["aaa", "bbb", "ccc"]);
        assert_eq!(resolve_pick("1", &s).as_deref(), Some("aaa"));
        assert_eq!(resolve_pick(" 2 \n", &s).as_deref(), Some("bbb")); // trims ws
        assert_eq!(resolve_pick("3", &s).as_deref(), Some("ccc"));
    }

    #[test]
    fn resolve_pick_cancels_on_blank_or_out_of_range() {
        let s = summaries(&["aaa", "bbb"]);
        assert_eq!(resolve_pick("", &s), None); // bare Enter = cancel
        assert_eq!(resolve_pick("   ", &s), None);
        assert_eq!(resolve_pick("abc", &s), None); // non-numeric = cancel
        assert_eq!(resolve_pick("0", &s), None); // menu is 1-based
        assert_eq!(resolve_pick("3", &s), None); // past the end
        assert_eq!(resolve_pick("-1", &s), None);
    }

    #[test]
    fn format_tool_list_groups_builtin_and_mcp() {
        let names = ["edit", "mcp__fs__read", "read_file", "mcp__fs__list"];
        let out = format_tool_list(&names);
        assert!(out.contains("工具（4）"), "shows total count: {out}");
        assert!(out.contains("内置（2）"));
        assert!(out.contains("edit"));
        assert!(out.contains("read_file"));
        assert!(out.contains("MCP（2）"));
        assert!(out.contains("mcp__fs__read"));
        assert!(out.contains("mcp__fs__list"));
    }

    #[test]
    fn format_tool_list_no_mcp_shows_none() {
        let names = ["read_file", "write_file"];
        let out = format_tool_list(&names);
        assert!(out.contains("工具（2）"));
        assert!(out.contains("MCP（0）：（无）"), "no-MCP marker: {out}");
    }

    #[test]
    fn doctor_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Doctor)));
        assert!(cli.model.is_none());
    }

    #[test]
    fn run_subcommand_parses_with_prompt() {
        let cli = Cli::try_parse_from(["orcarein", "run", "fix the bug"]).unwrap();
        match cli.command {
            Some(Command::Run { prompt, allow }) => {
                assert_eq!(prompt.as_deref(), Some("fix the bug"));
                assert!(allow.is_none());
            }
            other => panic!("expected run, got {other:?}"),
        }
        // "run" must not be mistaken for the positional model.
        assert!(cli.model.is_none());
    }

    #[test]
    fn run_subcommand_parses_without_prompt() {
        let cli = Cli::try_parse_from(["orcarein", "run"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                prompt: None,
                allow: None
            })
        ));
    }

    #[test]
    fn run_allow_flag_parses_csv() {
        let cli =
            Cli::try_parse_from(["orcarein", "run", "do it", "--allow", "bash,edit"]).unwrap();
        match cli.command {
            Some(Command::Run { prompt, allow }) => {
                assert_eq!(prompt.as_deref(), Some("do it"));
                assert_eq!(
                    allow.as_deref(),
                    Some(&["bash".to_owned(), "edit".to_owned()][..])
                );
            }
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn issue_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "issue", "42"]).unwrap();
        match cli.command {
            Some(Command::Issue { number }) => assert_eq!(number, 42),
            other => panic!("expected issue, got {other:?}"),
        }
        assert!(cli.model.is_none());
    }

    #[test]
    fn issue_subcommand_requires_a_number() {
        // A non-numeric issue id is rejected by clap.
        assert!(Cli::try_parse_from(["orcarein", "issue", "abc"]).is_err());
    }

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(30_000, 0), "30s ago");
        assert_eq!(format_age(120_000, 0), "2m ago");
        assert_eq!(format_age(7_200_000, 0), "2h ago");
        assert_eq!(format_age(2 * 86_400_000, 0), "2d ago");
        // A clock skew where "then" is in the future must not panic.
        assert_eq!(format_age(0, 5_000), "0s ago");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["orcarein", "--nope"]).is_err());
    }

    #[test]
    fn non_blank_treats_empty_as_absent() {
        // A set-but-empty env var or `--provider ""` must fall through, not
        // become a literal empty provider name.
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(non_blank(Some("   ".to_owned())), None);
        assert_eq!(
            non_blank(Some("openai".to_owned())),
            Some("openai".to_owned())
        );
    }

    #[cfg(not(feature = "tui"))]
    #[test]
    fn render_transcript_shows_turns_but_not_system_prompt() {
        let mut s = Session::new("be a helpful secret system prompt");
        s.push_user("hello");
        s.push_assistant(Message::assistant("hi there"));
        let t = render_transcript(&s);
        assert!(t.contains("hello"));
        assert!(t.contains("hi there"));
        // the system prompt is not part of the visible chat
        assert!(!t.contains("secret system prompt"));
    }

    #[cfg(not(feature = "tui"))]
    #[test]
    fn render_transcript_marks_an_empty_session() {
        let s = Session::new("sys");
        assert!(render_transcript(&s).contains("空"));
    }

    #[test]
    fn scope_key_bash_is_per_command() {
        assert_eq!(
            super::scope_key("bash", r#"{"command":"git status"}"#),
            "bash\u{0}git status"
        );
        assert_eq!(super::scope_key("edit", r#"{"path":"x"}"#), "edit");
        assert_eq!(super::scope_key("bash", "not json"), "bash");
    }

    #[test]
    fn persist_bash_rule_writes_targeted_rule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        super::persist_bash_rule(&path, r#"{"command":"git status"}"#);
        let cfg = orcarein_core::Config::load_from(&path).unwrap();
        let rules = &cfg.permissions.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].tool, "bash");
        assert_eq!(rules[0].command.as_deref(), Some("git status"));
        // Idempotent: writing the same rule again doesn't duplicate.
        super::persist_bash_rule(&path, r#"{"command":"git status"}"#);
        let cfg2 = orcarein_core::Config::load_from(&path).unwrap();
        assert_eq!(cfg2.permissions.unwrap().rules.len(), 1);
    }

    #[test]
    fn persist_bash_rule_never_clobbers_unparseable_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let corrupt = "this is not valid toml = = =";
        std::fs::write(&path, corrupt).unwrap();
        super::persist_bash_rule(&path, r#"{"command":"git status"}"#);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, corrupt, "corrupt config must be left untouched");
        assert!(!after.contains("[permissions]"));
    }

    #[test]
    fn classify_verify_reads_status_line_not_body() {
        // Ok → Verified.
        assert_eq!(super::classify_verify(None), super::VerifyOutcome::Verified);
        // A real auth rejection (status line carries 401) → Rejected.
        assert_eq!(
            super::classify_verify(Some(
                "models endpoint returned 401 Unauthorized:\n{\"error\":\"bad key\"}"
            )),
            super::VerifyOutcome::Rejected
        );
        // A 502 whose BODY mentions 401 must NOT be a false Rejected.
        assert_eq!(
            super::classify_verify(Some(
                "models endpoint returned 502 Bad Gateway:\n{\"error\":\"upstream 401\"}"
            )),
            super::VerifyOutcome::Inconclusive
        );
        // A network/timeout error → Inconclusive.
        assert_eq!(
            super::classify_verify(Some("models request timed out before response headers")),
            super::VerifyOutcome::Inconclusive
        );
    }

    #[test]
    fn env_overrides_stored_true_only_for_nonblank() {
        assert!(!super::env_overrides_stored(None));
        assert!(!super::env_overrides_stored(Some("")));
        assert!(!super::env_overrides_stored(Some("   ")));
        assert!(super::env_overrides_stored(Some("sk-abc")));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn history_doc_shows_turns_but_not_system_prompt() {
        let mut s = Session::new("be a helpful secret system prompt");
        s.push_user("hello");
        s.push_assistant(Message::assistant("hi **there**"));
        let doc = super::history_doc(&s, 80, color::ColorMode::None);
        let joined: String = doc
            .iter()
            .map(|l| l.plain.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("hello"));
        assert!(joined.contains("hi there")); // markdown bold markers stripped in plain
        assert!(!joined.contains("secret system prompt"));
        assert!(joined.contains("▌ 你") && joined.contains("▌ OrcaRein"));
    }

    #[cfg(feature = "hardware")]
    #[test]
    fn hw_monitor_subcommand_parses() {
        let cli = Cli::try_parse_from(["orcarein", "hw", "monitor", "--pins", "17,27", "--once"])
            .unwrap();
        match cli.command {
            Some(Command::Hw {
                action:
                    HwAction::Monitor {
                        pins,
                        once,
                        interval,
                    },
            }) => {
                assert_eq!(pins, vec![17, 27]);
                assert!(once);
                assert_eq!(interval, 500); // default
            }
            other => panic!("expected hw monitor, got {other:?}"),
        }
    }

    #[cfg(feature = "tui")]
    mod secret_keys {
        use crate::{secret_key_action, SecretAction};
        use ratatui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        #[test]
        fn release_events_are_ignored() {
            // Windows delivers Press AND Release per keystroke. Without this the
            // key doubles (sk-abcd -> sskk--aabbccdd) and 51 chars show 102 stars.
            // The filter existed in the loop as a comment with ZERO tests.
            assert_eq!(
                secret_key_action(
                    KeyEventKind::Release,
                    KeyCode::Char('a'),
                    KeyModifiers::NONE
                ),
                SecretAction::Ignore
            );
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Char('a'), KeyModifiers::NONE),
                SecretAction::Push('a')
            );
        }

        #[test]
        fn ctrl_c_cancels_and_is_not_the_letter_c() {
            // Arm order is load-bearing: if `Char(c)` were matched first, Ctrl+C
            // would silently push a 'c' into the key.
            assert_eq!(
                secret_key_action(
                    KeyEventKind::Press,
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL
                ),
                SecretAction::Cancel
            );
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Char('c'), KeyModifiers::NONE),
                SecretAction::Push('c')
            );
        }

        #[test]
        fn enter_submits_esc_cancels_backspace_pops() {
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Enter, KeyModifiers::NONE),
                SecretAction::Submit
            );
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Esc, KeyModifiers::NONE),
                SecretAction::Cancel
            );
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Backspace, KeyModifiers::NONE),
                SecretAction::Pop
            );
        }

        #[test]
        fn other_keys_do_nothing() {
            assert_eq!(
                secret_key_action(KeyEventKind::Press, KeyCode::Left, KeyModifiers::NONE),
                SecretAction::Ignore
            );
        }

        #[test]
        fn other_control_chords_are_ignored_not_typed() {
            // Ctrl+U is "kill line" muscle memory; without the guard it pushed a 'u'.
            for c in ['u', 'd', 'a', 'w'] {
                assert_eq!(
                    secret_key_action(KeyEventKind::Press, KeyCode::Char(c), KeyModifiers::CONTROL),
                    SecretAction::Ignore,
                    "Ctrl+{c} must not type a character"
                );
            }
        }

        #[test]
        fn altgr_chars_still_type() {
            // Windows reports AltGr as CTRL+ALT. A German keyboard types '@' that way,
            // and API keys contain such characters.
            assert_eq!(
                secret_key_action(
                    KeyEventKind::Press,
                    KeyCode::Char('@'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                ),
                SecretAction::Push('@')
            );
        }
    }
}
