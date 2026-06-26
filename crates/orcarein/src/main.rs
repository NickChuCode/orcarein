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
    CacheMode, Config, Decision, DeepSeekProvider, EditTool, EventSink, ListDirTool,
    OpenAIProvider, PermissionPolicy, PermissionStore, Provider, ReadFileTool, RiskLevel,
    SearchTool, SecretStore, Session, SessionStore, SessionSummary, Tool, ToolRegistry,
    WriteFileTool, MAX_TOOL_ITERATIONS,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

#[cfg(feature = "hardware")]
mod hwmon;
mod overlay;

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
    provider: Box<dyn Provider>,
    model: String,
    system_prompt: String,
    tools_allowlist: Option<Vec<String>>,
    config_path: Option<PathBuf>,
}

/// Builds a `Provider` from a resolved name + optional API key. Validates the
/// provider name first so a missing key never masks a typo'd provider.
fn build_provider(name: &str, api_key: Option<String>) -> Result<Box<dyn Provider>> {
    match name {
        "deepseek" | "openai" => {}
        other => bail!("unknown provider: '{other}' (expected: deepseek | openai)"),
    }
    let var = env_key_var(name).expect("provider validated above");
    let key = api_key.with_context(|| {
        format!(
            "no API key for provider '{name}'. In PowerShell: $env:{var} = '<your-key>' \
             (or store it once in secrets.toml)"
        )
    })?;
    match name {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(key))),
        "openai" => Ok(Box::new(OpenAIProvider::new(key))),
        _ => unreachable!("provider validated above"),
    }
}

/// Treats a blank string as absent, so `--provider ""` or `ORCAREIN_PROVIDER=`
/// (set-but-empty) fall through to the next precedence layer rather than
/// becoming a literal empty value.
fn non_blank(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

/// Resolves effective settings from the precedence chain
/// CLI flag > env var > `config.toml` > built-in default.
fn resolve(cli: &Cli) -> Result<Resolved> {
    let config = Config::load().context("failed to load config.toml")?;

    let provider_name = non_blank(cli.provider.clone())
        .or_else(|| non_blank(std::env::var("ORCAREIN_PROVIDER").ok()))
        .or_else(|| non_blank(config.provider.clone()))
        .unwrap_or_else(|| "deepseek".to_owned());

    let secrets = SecretStore::load().context("failed to load secrets.toml")?;
    let provider = build_provider(&provider_name, secrets.resolve(&provider_name))?;

    let model = non_blank(cli.model.clone())
        .or_else(|| non_blank(config.model.clone()))
        .unwrap_or_else(|| provider.default_model().to_owned());

    let tools_allowlist = cli.tools.clone().or_else(|| config.tools_allowlist.clone());

    let system_prompt = match &cli.system_prompt_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read --system-prompt-file {}", path.display()))?,
        None => config
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_owned()),
    };

    Ok(Resolved {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        config_path: Config::config_path(),
    })
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

    let resolved = resolve(&cli)?;
    let Resolved {
        provider,
        model,
        system_prompt,
        tools_allowlist,
        config_path,
    } = resolved;

    // Either continue the resumed session (keeping its id + creation time so
    // auto-save writes back to the same file) or start a fresh one.
    let (mut session, session_id, created_at_ms) = match resumed {
        Some((loaded, id, created)) => {
            println!("Resumed session {id} ({} turns).", loaded.turn_count());
            (loaded, id, created)
        }
        None => {
            let created = SessionStore::now_ms();
            (Session::new(&system_prompt), created.to_string(), created)
        }
    };
    let mut editor = DefaultEditor::new().context("failed to start the line editor")?;

    #[cfg_attr(not(feature = "mcp"), allow(unused_mut))]
    let mut registry = build_registry(tools_allowlist.as_deref());
    #[cfg(feature = "mcp")]
    let _mcp_clients = {
        let mcp_cfg = orcarein_core::Config::load().unwrap_or_default();
        orcarein_core::mcp::setup_servers(&mcp_cfg.mcp_servers, &mut registry).await
    };
    let tool_defs = registry.definitions();

    println!("OrcaRein — chat with {model}. /help for commands, Ctrl+D to quit.");
    println!("Provider: {}", provider.name());
    println!(
        "Config: {}",
        config_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_owned())
    );
    println!("Tools: {}", registry.names().join(", "));
    println!(
        "Session: {session_id} (auto-saved to {})",
        store.path_for(&session_id).display()
    );
    if cli.no_permission {
        println!("Permissions: DISABLED (--no-permission)\n");
    } else {
        println!(
            "Permissions: prompt on Risky tools (use --no-permission with piped input to skip)\n"
        );
    }

    if cli.no_economy {
        println!("Cache: economy OFF (benchmark — prefix cache deliberately defeated)\n");
    }

    // The agent loop now lives in `orcarein-core`; the REPL is a thin frontend
    // that supplies an interactive permission policy and a printing event sink.
    let agent =
        Agent::new(provider.as_ref(), &registry, &tool_defs).with_cache_mode(cache_mode(&cli));
    let mut policy: Box<dyn PermissionPolicy> = if cli.no_permission {
        Box::new(AllowlistPolicy::allow_all())
    } else {
        Box::new(InteractivePolicy::new())
    };

    // The prompt-token count of the most recent turn ≈ current context fill;
    // surfaced in the per-turn meter and `/usage`. 0 until the first turn.
    let mut last_prompt_tokens: u64 = 0;

    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(e).context("line editor failed"),
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
            ) {
                CommandAction::Continue => continue,
                CommandAction::Quit => break,
            }
        }

        let _ = editor.add_history_entry(input);
        session.push_user(input);

        let mut sink = ReplSink::new();
        match agent
            .run_turn(&mut session, &model, policy.as_mut(), &mut sink)
            .await
        {
            Ok(outcome) => {
                println!(); // close the final streamed line
                let total = session.usage();
                last_prompt_tokens = outcome.usage.prompt_tokens;
                let ctx = cost::context_line(last_prompt_tokens, &model)
                    .map(|c| format!(" | {c}"))
                    .unwrap_or_default();
                let meter = cost::meter_line(&total, &model)
                    .map(|m| format!(" | {m}"))
                    .unwrap_or_default();
                eprintln!(
                    "[tokens: +{} this turn / {} total{}{}]\n",
                    outcome.usage.total_tokens, total.total_tokens, ctx, meter
                );
                // Auto-save after a successful turn; never let a save error
                // interrupt the conversation.
                if let Err(e) = store.save(&session_id, created_at_ms, &session) {
                    eprintln!("[warn] 自动保存失败：{e}");
                }
            }
            Err(e) => {
                eprintln!("\n[错误] {e:#}\n");
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
}

impl InteractivePolicy {
    fn new() -> Self {
        InteractivePolicy {
            store: PermissionStore::new(),
        }
    }
}

impl PermissionPolicy for InteractivePolicy {
    fn decide(&mut self, tool: &str, args: &str, _risk: RiskLevel) -> Decision {
        if let Some(d) = self.store.cached(tool) {
            return d;
        }
        let d = prompt_permission(tool, args);
        if d.is_sticky() {
            self.store.remember(tool, d);
        }
        d
    }
}

/// Renders [`AgentEvent`]s the way the REPL always has: reasoning/content to
/// stdout under `[思考]`/`[回复]` headers, tool activity to stderr.
struct ReplSink {
    started_reasoning: bool,
    started_content: bool,
}

impl ReplSink {
    fn new() -> Self {
        ReplSink {
            started_reasoning: false,
            started_content: false,
        }
    }
}

impl EventSink for ReplSink {
    fn emit(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Reasoning(text) => {
                if !self.started_reasoning {
                    println!("[思考]");
                    self.started_reasoning = true;
                }
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::Content(text) => {
                if !self.started_content {
                    if self.started_reasoning {
                        println!("\n");
                    }
                    println!("[回复]");
                    self.started_content = true;
                }
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ToolStarted {
                name, arguments, ..
            } => {
                if self.started_content || self.started_reasoning {
                    println!();
                }
                eprintln!("[tool: {name}({arguments})]");
            }
            AgentEvent::ToolFinished {
                result, is_error, ..
            } => {
                if is_error {
                    eprintln!("[tool error] {result}");
                } else {
                    eprintln!("[result] {} bytes", result.len());
                }
                // The next model response is a fresh segment.
                self.started_reasoning = false;
                self.started_content = false;
            }
            AgentEvent::Usage(_) => {} // printed once at end of turn
            AgentEvent::IterationLimit => {
                eprintln!("[超过 tool call 上限 {MAX_TOOL_ITERATIONS} 次，中断]");
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
        ..
    } = resolved;

    #[cfg_attr(not(feature = "mcp"), allow(unused_mut))]
    let mut registry = build_registry(tools_allowlist.as_deref());
    #[cfg(feature = "mcp")]
    let _mcp_clients = {
        let mcp_cfg = orcarein_core::Config::load().unwrap_or_default();
        orcarein_core::mcp::setup_servers(&mcp_cfg.mcp_servers, &mut registry).await
    };
    let tool_defs = registry.definitions();
    let agent =
        Agent::new(provider.as_ref(), &registry, &tool_defs).with_cache_mode(cache_mode(cli));
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
/// line, EOF, IO error — collapses to `DenyOnce` (deny-by-default).
fn prompt_permission(name: &str, args: &str) -> Decision {
    eprintln!();
    eprintln!("OrcaRein wants to run: {name}({args})");
    eprint!("Allow? [y=once N=never A=always n=once]: ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Decision::DenyOnce;
    }
    match line.trim().chars().next() {
        Some('y') => Decision::AllowOnce,
        Some('A') => Decision::AllowAlways,
        Some('N') => Decision::DenyAlways,
        _ => Decision::DenyOnce,
    }
}

/// Renders a session's messages to plain text for the `/history` pager. Pure
/// and read-only — viewing history must never *become* history: the persisted
/// `Vec<Message>` (and thus the model's cache prefix) is left untouched.
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

/// `/show <path>`: reads a file and shows it through the pager. Read failures
/// are reported, not fatal.
fn run_show(path: &str) {
    if path.is_empty() {
        eprintln!("用法：/show <文件路径>");
        return;
    }
    match std::fs::read_to_string(path) {
        Ok(content) => {
            if let Err(e) = overlay::show_paged(path, &content) {
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
            // when the model is known.
            match cost::context_line(last_prompt_tokens, model) {
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
        "show" => {
            run_show(arg);
            CommandAction::Continue
        }
        "history" => {
            let transcript = render_transcript(session);
            if let Err(e) = overlay::show_paged("对话记录", &transcript) {
                eprintln!("显示失败：{e}");
            }
            CommandAction::Continue
        }
        "help" => {
            println!("命令：");
            println!("  /exit, /quit   退出");
            println!("  /clear         清空会话（保留 system prompt）");
            println!("  /save          立即保存会话到磁盘（每轮也会自动保存）");
            println!("  /usage, /context  token 用量 + 上下文占用 + 成本");
            println!("  /tools         列出当前可用工具（内置 + MCP）");
            println!("  /show <文件>   分页查看一个文件（长则进浮层，q 退出）");
            println!("  /history       分页查看本次对话记录");
            println!("  /help          这条帮助");
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
        provider, model, ..
    } = resolve(cli)?;

    // 5. Restricted toolset: read/list/search/edit/write only — NO shell.
    let issue_tools: Vec<String> = ["read_file", "list_dir", "search", "edit", "write_file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let registry = build_registry(Some(&issue_tools));
    let tool_defs = registry.definitions();
    let agent = Agent::new(provider.as_ref(), &registry, &tool_defs)
        .with_cache_mode(cache_mode(cli))
        .with_max_iterations(ISSUE_MAX_ITERATIONS);

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

    #[test]
    fn render_transcript_marks_an_empty_session() {
        let s = Session::new("sys");
        assert!(render_transcript(&s).contains("空"));
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
}
