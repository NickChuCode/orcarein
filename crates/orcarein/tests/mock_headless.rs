//! Headless integration tests: drive `orcarein run --provider mock` with a
//! scripted provider and assert on captured output. No PTY — reliable on all
//! platforms, runs in the main --all-features matrix. Gated on mock-provider so
//! it vanishes from the default build.
//!
//! Hermetic: `run_mock_in` points the child's ORCAREIN_CONFIG_DIR at a fresh
//! empty tempdir, so it reads neither the dev's config.toml (no [permissions]
//! rule can flip a deny) nor secrets.toml (no real key = no billing). Config-
//! rule scenarios inject a test config.toml via the same dir.
#![cfg(feature = "mock-provider")]

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Like `run_mock` but runs the child in `cwd` (so a tool that touches the
/// filesystem resolves relative paths there). `run_mock` passes the current dir.
///
/// `--provider` and `--no-permission` are top-level `Cli` flags without
/// `global = true`, so clap only accepts them *before* the `run` subcommand
/// (confirmed against the real binary: `run <prompt> --provider mock` errors
/// with "unexpected argument '--provider' found"). `--permission-mode` is
/// `global = true` and would parse on either side, but `extra_args` goes
/// before `run` too so one flag order covers every scenario.
fn run_mock_in(
    script_json: &str,
    prompt: &str,
    extra_args: &[&str],
    cwd: Option<&Path>,
    config_toml: Option<&str>,
) -> String {
    let mut script = tempfile::NamedTempFile::new().expect("temp script");
    script.write_all(script_json.as_bytes()).unwrap();
    script.flush().unwrap();

    // Hermetic config dir: always a fresh empty tempdir → the child reads no
    // dev config.toml / secrets.toml (no dev [permissions] can flip a deny; no
    // real API key = no billing). Optionally seed a test config.toml.
    let cfg_dir = tempfile::tempdir().expect("temp config dir");
    if let Some(toml) = config_toml {
        std::fs::write(cfg_dir.path().join("config.toml"), toml).expect("write test config");
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orcarein"));
    cmd.arg("--provider")
        .arg("mock")
        .args(extra_args)
        .arg("run")
        .arg(prompt)
        .env("ORCAREIN_MOCK_SCRIPT", script.path())
        .env("ORCAREIN_PROVIDER", "mock")
        .env("ORCAREIN_CONFIG_DIR", cfg_dir.path())
        .env("NO_COLOR", "1");
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd.output().expect("spawn orcarein");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Run `orcarein --provider mock <extra_args> run <prompt>` with `script_json`
/// as the mock script, in the current dir. Returns combined stdout+stderr.
fn run_mock(script_json: &str, prompt: &str, extra_args: &[&str]) -> String {
    run_mock_in(script_json, prompt, extra_args, None, None)
}

#[test]
fn mock_run_echoes_scripted_text() {
    let out = run_mock(r#"[{"text":"MOCK_MARKER_OK"}]"#, "hi", &[]);
    assert!(out.contains("MOCK_MARKER_OK"), "output:\n{out}");
}

#[test]
fn plan_denies_edit() {
    // plan hides edit from the model AND the ceiling denies it at the gate.
    let script = r#"[{"tools":[{"name":"edit","args":"{\"path\":\"x.txt\",\"old\":\"a\",\"new\":\"b\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "change x", &["--permission-mode", "plan"]);
    assert!(
        out.contains("permission denied"),
        "plan must deny edit:\n{out}"
    );
}

#[test]
fn plan_denies_bash() {
    let script =
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "run it", &["--permission-mode", "plan"]);
    assert!(
        out.contains("permission denied"),
        "plan must deny bash:\n{out}"
    );
}

#[test]
fn headless_default_denies_bash() {
    // `run` without --allow is deny_all for Risky tools.
    let script =
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "run it", &[]);
    assert!(
        out.contains("permission denied"),
        "default headless must deny bash:\n{out}"
    );
}

#[test]
fn yolo_allows_bash_headless() {
    // yolo lifts the Ask -> bash runs, no denial. (echo is harmless.)
    let script =
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "run it", &["--permission-mode", "yolo"]);
    assert!(
        !out.contains("permission denied"),
        "yolo must not deny bash:\n{out}"
    );
    assert!(
        out.contains("[tool ok]"),
        "yolo must actually run bash:\n{out}"
    );
}

#[test]
fn no_permission_prints_deprecation() {
    let out = run_mock(r#"[{"text":"ok"}]"#, "hi", &["--no-permission"]);
    assert!(
        out.contains("deprecated"),
        "expected deprecation warning:\n{out}"
    );
}

#[test]
fn accept_edits_allows_edit() {
    // acceptEdits lets an ordinary (non-sensitive) edit through without a prompt.
    // The edit then really runs, so the target must exist — stage x.txt="a" in a
    // temp cwd; a successful edit prints "[tool ok]" (a real positive assertion).
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("x.txt"), "a").unwrap();
    let script = r#"[{"tools":[{"name":"edit","args":"{\"path\":\"x.txt\",\"old_str\":\"a\",\"new_str\":\"b\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(
        script,
        "change x",
        &["--permission-mode", "acceptEdits"],
        Some(dir.path()),
        None,
    );
    assert!(
        !out.contains("permission denied"),
        "acceptEdits must not deny edit:\n{out}"
    );
    assert!(
        out.contains("[tool ok]"),
        "acceptEdits must actually run edit:\n{out}"
    );
}

#[test]
fn sensitive_path_denied_headless() {
    // Reading .env hits the built-in sensitive-path default (Ask); under the
    // headless deny_all policy that becomes a denial. No real file needed — the
    // gate reads the path from the tool args.
    let script =
        r#"[{"tools":[{"name":"read_file","args":"{\"path\":\".env\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "read env", &[]);
    assert!(
        out.contains("permission denied"),
        "sensitive .env read must be denied:\n{out}"
    );
}

#[test]
fn accept_edits_denies_bash() {
    // acceptEdits only lifts edit/write_file; bash still Asks → deny_all denies.
    let script =
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(
        script,
        "run it",
        &["--permission-mode", "acceptEdits"],
        None,
        None,
    );
    assert!(
        out.contains("permission denied"),
        "acceptEdits must still deny bash:\n{out}"
    );
}

#[test]
fn accept_edits_denies_sensitive_edit() {
    // acceptEdits does NOT eat the sensitive-path default: editing .env still Asks → deny.
    // (denied at the gate before execute, so the edit args are never validated.)
    let script = r#"[{"tools":[{"name":"edit","args":"{\"path\":\".env\",\"old_str\":\"a\",\"new_str\":\"b\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(
        script,
        "edit env",
        &["--permission-mode", "acceptEdits"],
        None,
        None,
    );
    assert!(
        out.contains("permission denied"),
        "acceptEdits must still deny sensitive edit:\n{out}"
    );
}

#[test]
fn yolo_allows_sensitive_read() {
    // yolo lifts every Ask incl. the sensitive default. Stage .env so the read
    // succeeds → [tool ok] (real positive, not just "not denied").
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".env"), "SECRET=x").unwrap();
    let script =
        r#"[{"tools":[{"name":"read_file","args":"{\"path\":\".env\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(
        script,
        "read env",
        &["--permission-mode", "yolo"],
        Some(dir.path()),
        None,
    );
    assert!(
        !out.contains("permission denied"),
        "yolo must not deny sensitive read:\n{out}"
    );
    assert!(
        out.contains("[tool ok]"),
        "yolo must actually read .env:\n{out}"
    );
}

#[test]
fn sensitive_ssh_path_denied() {
    // A read of a key-shaped path (id_rsa) hits the sensitive default → deny_all
    // denies headless. Path comes from args; no real file needed.
    let script =
        r#"[{"tools":[{"name":"read_file","args":"{\"path\":\"id_rsa\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "read key", &[]);
    assert!(
        out.contains("permission denied"),
        "reading id_rsa must be denied:\n{out}"
    );
}

#[test]
fn plan_search_env_denied() {
    // plan whitelists search (read-only) but the sensitive default (step ③)
    // fires before the plan whitelist (step ④): searching .env still denies.
    let script = r#"[{"tools":[{"name":"search","args":"{\"pattern\":\"x\",\"path\":\".env\"}"}]},{"text":"done"}]"#;
    let out = run_mock(script, "search env", &["--permission-mode", "plan"]);
    assert!(
        out.contains("permission denied"),
        "plan must still deny sensitive search:\n{out}"
    );
}

#[test]
fn explicit_allow_beats_sensitive() {
    // A user allow rule for .env wins over the sensitive-path default → the read
    // is allowed even headless. Stage .env so it succeeds → [tool ok].
    let cfg = r#"
[[permissions.rules]]
tool = "*"
path = ".env"
action = "allow"
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".env"), "SECRET=x").unwrap();
    let script =
        r#"[{"tools":[{"name":"read_file","args":"{\"path\":\".env\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(script, "read env", &[], Some(dir.path()), Some(cfg));
    assert!(
        !out.contains("permission denied"),
        "explicit allow must beat sensitive default:\n{out}"
    );
    assert!(out.contains("[tool ok]"), "allowed read must run:\n{out}");
}

#[test]
fn explicit_deny_survives_yolo() {
    // yolo only lifts Ask, never Deny. A config deny on bash stands even in yolo.
    let cfg = r#"
[[permissions.rules]]
tool = "bash"
action = "deny"
"#;
    let script =
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#;
    let out = run_mock_in(
        script,
        "run it",
        &["--permission-mode", "yolo"],
        None,
        Some(cfg),
    );
    assert!(
        out.contains("permission denied"),
        "config deny must survive yolo:\n{out}"
    );
}

#[test]
fn command_glob_allow_and_deny() {
    // Command-scoped bash rules: allow `echo *`, deny the more specific
    // `echo secret*`. echo hi runs; echo secret is denied.
    let cfg = r#"
[[permissions.rules]]
tool = "bash"
command = "echo *"
action = "allow"

[[permissions.rules]]
tool = "bash"
command = "echo secret*"
action = "deny"
"#;
    let allowed = run_mock_in(
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo hi\"}"}]},{"text":"done"}]"#,
        "echo hi",
        &[],
        None,
        Some(cfg),
    );
    assert!(
        allowed.contains("[tool ok]"),
        "echo hi (allow rule) must run:\n{allowed}"
    );
    assert!(
        !allowed.contains("permission denied"),
        "echo hi must not be denied:\n{allowed}"
    );

    let denied = run_mock_in(
        r#"[{"tools":[{"name":"bash","args":"{\"command\":\"echo secret\"}"}]},{"text":"done"}]"#,
        "echo secret",
        &[],
        None,
        Some(cfg),
    );
    assert!(
        denied.contains("permission denied"),
        "echo secret (deny rule) must be denied:\n{denied}"
    );
}
