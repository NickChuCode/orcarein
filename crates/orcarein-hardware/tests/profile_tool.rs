//! Integration tests for `ProfileTool` — Task 6.
//!
//! Test coverage:
//! 1. `native_i2c_scan`        — native backend, MockTransport, live dispatch.
//! 2. `dry_run_python`         — python backend, dry_run=true, no sidecar dereferenced.
//! 3. `out_of_range_rejected`  — validation fires before any side effect.
//! 4. `risk_and_schema`        — risk_level / name / schema come from Intent.
//! 5. `python_end_to_end`      — real sidecar (python-gated).

use std::sync::Arc;

use orcarein_core::{RiskLevel, Tool};
use orcarein_hardware::{
    profile::Profile,
    sidecar::Sidecar,
    tool::{Executor, ProfileTool},
    transport::MockTransport,
};
use serde_json::json;
use std::path::PathBuf;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Shared TOML snippets
// ---------------------------------------------------------------------------

/// A profile with one native `i2c_scan` intent (no params).
const SCAN_PROFILE: &str = r#"
schema_version = 1
[device]
name = "test_device"
description = "test device"
transport = "i2c"
i2c_bus = 1
i2c_addr = 0x40

[[intent]]
name = "i2c_scan"
description = "scan the i2c bus"
risk = "safe"
backend = "native"
[intent.native]
op = "i2c_scan"
"#;

/// A profile with one python `set_joint` intent (joint:int 0..5, angle:int 0..180).
const SET_JOINT_PROFILE: &str = r#"
schema_version = 1
[device]
name = "arm"
description = "servo arm"
transport = "i2c"
i2c_bus = 1

[[intent]]
name = "set_joint"
description = "set a joint angle"
risk = "risky"
backend = "python"
[[intent.param]]
name = "joint"
type = "int"
min = 0
max = 5
[[intent.param]]
name = "angle"
type = "int"
min = 0
max = 180
[intent.python]
call = "servo.servo[{joint}].angle = {angle}"
"#;

/// A profile with python `record` (exec) and `last` (eval) intents for
/// the end-to-end python test, backed by a simple `log = []` init.
const PYTHON_E2E_PROFILE: &str = r#"
schema_version = 1
[device]
name = "log_device"
description = "in-memory log device"
transport = "none"
[device.python]
init = "log = []"

[[intent]]
name = "record"
description = "append a value to the log"
risk = "risky"
backend = "python"
[[intent.param]]
name = "v"
type = "int"
min = 0
max = 1000
[intent.python]
call = "log.append({v})"

[[intent]]
name = "last"
description = "return the last element of the log"
risk = "safe"
backend = "python"
[intent.python]
call = "log[-1]"
returns = "int"
"#;

// ---------------------------------------------------------------------------
// Helper: path to the bundled sidecar script.
// ---------------------------------------------------------------------------

fn sidecar_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("python")
        .join("sidecar.py")
}

// ---------------------------------------------------------------------------
// Test 1: native i2c_scan dispatches through MockTransport.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn native_i2c_scan() {
    let profile = Profile::from_toml_str(SCAN_PROFILE).expect("valid scan profile");
    let intent = profile.intents.into_iter().next().expect("one intent");

    let mock = Arc::new(MockTransport::new().with_scan_result("0x40"));
    let exec = Arc::new(Executor {
        transport: mock.clone() as Arc<dyn orcarein_hardware::transport::Transport>,
        sidecar: None,
        dry_run: false,
    });

    let tool = ProfileTool::new(intent, exec);
    let out = tool.execute(json!({})).await.expect("execute succeeded");
    assert_eq!(out.content, "0x40");
}

// ---------------------------------------------------------------------------
// Test 2: dry-run python backend — no sidecar is dereferenced.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dry_run_python() {
    let profile = Profile::from_toml_str(SET_JOINT_PROFILE).expect("valid set_joint profile");
    let intent = profile
        .intents
        .into_iter()
        .find(|i| i.name == "set_joint")
        .expect("set_joint intent");

    let exec = Arc::new(Executor {
        transport: Arc::new(MockTransport::new()),
        sidecar: None, // no sidecar — dry-run must NOT deref this
        dry_run: true,
    });

    let tool = ProfileTool::new(intent, exec);
    let out = tool
        .execute(json!({"joint": 2, "angle": 90}))
        .await
        .expect("dry-run succeeded");

    // Must announce itself as DRY-RUN.
    assert!(
        out.content.starts_with("DRY-RUN"),
        "output should start with DRY-RUN, got: {:?}",
        out.content
    );
    // Rendered code should be visible in the output.
    assert!(
        out.content.contains("servo.servo[2].angle = 90"),
        "output should contain rendered call, got: {:?}",
        out.content
    );
}

// ---------------------------------------------------------------------------
// Test 3: out-of-range arg is rejected before any side effect fires.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn out_of_range_rejected() {
    let profile = Profile::from_toml_str(SET_JOINT_PROFILE).expect("valid set_joint profile");
    let intent = profile
        .intents
        .into_iter()
        .find(|i| i.name == "set_joint")
        .expect("set_joint intent");

    let mock = Arc::new(MockTransport::new());
    let exec = Arc::new(Executor {
        transport: mock.clone() as Arc<dyn orcarein_hardware::transport::Transport>,
        sidecar: None,
        dry_run: false,
    });

    let tool = ProfileTool::new(intent, exec);
    // joint=9 is out of range (max=5) — should fail.
    let err = tool
        .execute(json!({"joint": 9, "angle": 90}))
        .await
        .expect_err("should fail for out-of-range joint");

    // Error should be ToolError::Other (converted from HardwareError::Validation).
    assert!(
        matches!(err, orcarein_core::ToolError::Other(_)),
        "expected ToolError::Other, got: {err:?}"
    );

    // Validation must happen BEFORE any transport call.
    assert!(
        mock.recorded().is_empty(),
        "no transport ops should be recorded for a validation error"
    );
}

// ---------------------------------------------------------------------------
// Test 4: risk_level, name, and schema mirror the Intent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn risk_and_schema() {
    let profile = Profile::from_toml_str(SET_JOINT_PROFILE).expect("valid set_joint profile");
    let intent = profile
        .intents
        .into_iter()
        .find(|i| i.name == "set_joint")
        .expect("set_joint intent");

    let exec = Arc::new(Executor {
        transport: Arc::new(MockTransport::new()),
        sidecar: None,
        dry_run: true,
    });

    let tool = ProfileTool::new(intent, exec);

    assert_eq!(tool.name(), "set_joint");
    assert_eq!(tool.risk_level(), RiskLevel::Risky);

    let schema = tool.schema();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["joint"].is_object());
    assert!(schema["properties"]["angle"].is_object());
}

// ---------------------------------------------------------------------------
// Test 5: python end-to-end (python-gated).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn python_end_to_end() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("skipping python_end_to_end: no python interpreter found");
        return;
    };

    let profile = Profile::from_toml_str(PYTHON_E2E_PROFILE).expect("valid python e2e profile");

    // Spawn a real sidecar with the init snippet from the profile.
    let init = profile.device.python_init.as_deref();
    let sidecar = Sidecar::spawn(&py, &sidecar_script(), init)
        .await
        .expect("sidecar spawn failed");

    let exec = Arc::new(Executor {
        transport: Arc::new(MockTransport::new()),
        sidecar: Some(Mutex::new(sidecar)),
        dry_run: false,
    });

    // Find the two intents.
    let record_intent = profile
        .intents
        .iter()
        .find(|i| i.name == "record")
        .expect("record intent")
        .clone();
    let last_intent = profile
        .intents
        .iter()
        .find(|i| i.name == "last")
        .expect("last intent")
        .clone();

    let record_tool = ProfileTool::new(record_intent, exec.clone());
    let last_tool = ProfileTool::new(last_intent, exec.clone());

    // Append 7 to the log.
    let append_out = record_tool
        .execute(json!({"v": 7}))
        .await
        .expect("record execute failed");
    assert_eq!(append_out.content, "ok");

    // Read the last element; should be 7.
    let last_out = last_tool
        .execute(json!({}))
        .await
        .expect("last execute failed");
    assert_eq!(last_out.content, "7");
}
