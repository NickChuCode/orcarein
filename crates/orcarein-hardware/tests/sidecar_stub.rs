//! Integration tests for [`orcarein_hardware::sidecar::Sidecar`].
//!
//! Each test gates on Python being available via
//! [`Sidecar::locate_python`].  When no Python interpreter is found the test
//! prints a skip message and returns vacuously — CI environments without
//! Python remain green.

use std::path::PathBuf;

use orcarein_hardware::sidecar::Sidecar;
use orcarein_hardware::HardwareError;

/// Absolute path to the bundled `python/sidecar.py` inside this crate.
fn script_path() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("python").join("sidecar.py")
}

// ---------------------------------------------------------------------------
// Test: init code is executed in the persistent namespace
// ---------------------------------------------------------------------------

/// Spawn with `init = Some("x = 41")`, then `eval("x + 1")` must return 42.
#[tokio::test]
async fn init_sets_namespace_variable() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("skipping: no python found");
        return;
    };

    let mut sidecar = Sidecar::spawn(&py, &script_path(), Some("x = 41"))
        .await
        .expect("spawn failed");

    let result = sidecar.eval("x + 1".to_owned()).await.expect("eval failed");

    assert_eq!(result, serde_json::json!(42));
}

// ---------------------------------------------------------------------------
// Test: namespace persists across exec + eval calls
// ---------------------------------------------------------------------------

/// Spawn without init, `exec("y = 7")`, then `eval("y * 2")` must return 14.
/// Proves the namespace survives between separate requests.
#[tokio::test]
async fn namespace_persists_across_calls() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("skipping: no python found");
        return;
    };

    let mut sidecar = Sidecar::spawn(&py, &script_path(), None)
        .await
        .expect("spawn failed");

    sidecar.exec("y = 7".to_owned()).await.expect("exec failed");

    let result = sidecar.eval("y * 2".to_owned()).await.expect("eval failed");

    assert_eq!(result, serde_json::json!(14));
}

// ---------------------------------------------------------------------------
// Test: Python exceptions surface as HardwareError::Sidecar
// ---------------------------------------------------------------------------

/// Evaluating an undefined name must yield `Err(HardwareError::Sidecar(_))`
/// carrying the Python error text.
#[tokio::test]
async fn python_exception_becomes_sidecar_error() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("skipping: no python found");
        return;
    };

    let mut sidecar = Sidecar::spawn(&py, &script_path(), None)
        .await
        .expect("spawn failed");

    let err = sidecar
        .eval("nonexistent_var".to_owned())
        .await
        .expect_err("expected an error for undefined variable");

    assert!(
        matches!(err, HardwareError::Sidecar(_)),
        "expected HardwareError::Sidecar, got: {err:?}"
    );

    // The error message should mention the Python NameError.
    let HardwareError::Sidecar(msg) = err else {
        unreachable!()
    };
    assert!(
        msg.contains("NameError"),
        "error text should mention NameError, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Test: stderr output in init block does not hang the sidecar
// ---------------------------------------------------------------------------

/// Spawn with an `init` block that writes to stderr, then `eval("z")` must
/// return 5.  Verifies that the stderr-drain task prevents the pipe-buffer
/// deadlock that would otherwise block a chatty child.
#[tokio::test]
async fn stderr_in_init_does_not_hang() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("skipping: no python found");
        return;
    };

    let mut sidecar = Sidecar::spawn(
        &py,
        &script_path(),
        Some("import sys; sys.stderr.write('warn\\n'); z = 5"),
    )
    .await
    .expect("spawn failed");

    let result = sidecar.eval("z".to_owned()).await.expect("eval failed");

    assert_eq!(result, serde_json::json!(5));
}
