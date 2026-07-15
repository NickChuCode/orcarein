//! Headless integration tests: drive `orcarein run --provider mock` with a
//! scripted provider and assert on captured output. No PTY — reliable on all
//! platforms, runs in the main --all-features matrix. Gated on mock-provider so
//! it vanishes from the default build.
//!
//! Hermeticity note: `run_once` calls `Config::load()`, so these spawn a child
//! that reads the ambient `config.toml`. On CI runners (no config) that is
//! hermetic by absence and deterministic. On a dev box a permissive
//! `[permissions]` rule could in principle flip a deny assertion (and an
//! `[mcp_servers]` block would spawn those servers). A test-only config-dir
//! override is a deferred follow-up — see `notes/book/src/v02-41-e2e-qa-harness.md`.
#![cfg(feature = "mock-provider")]

use std::io::Write;
use std::process::Command;

/// Run `orcarein --provider mock <extra_args> run <prompt>` with `script_json`
/// as the mock script. Returns combined stdout+stderr.
///
/// `--provider` and `--no-permission` are top-level `Cli` flags without
/// `global = true`, so clap only accepts them *before* the `run` subcommand
/// (confirmed against the real binary: `run <prompt> --provider mock` errors
/// with "unexpected argument '--provider' found"). `--permission-mode` is
/// `global = true` and would parse on either side, but `extra_args` goes
/// before `run` too so one flag order covers every scenario.
fn run_mock(script_json: &str, prompt: &str, extra_args: &[&str]) -> String {
    let mut script = tempfile::NamedTempFile::new().expect("temp script");
    script.write_all(script_json.as_bytes()).unwrap();
    script.flush().unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_orcarein"))
        .arg("--provider")
        .arg("mock")
        .args(extra_args)
        .arg("run")
        .arg(prompt)
        .env("ORCAREIN_MOCK_SCRIPT", script.path())
        .env("ORCAREIN_PROVIDER", "mock")
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn orcarein");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
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
