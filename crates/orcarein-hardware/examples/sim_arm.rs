//! Hardware-free, end-to-end demo of the M1 hardware-control stack.
//!
//! Drives a **simulated** 6-DOF servo arm (pure Python, no GPIO / no hardware)
//! through the exact same pipeline the real product will use:
//! device profile -> `ProfileTool` (an `orcarein_core::Tool`) -> persistent
//! Python sidecar. This is what M2's `orcarein hw` subcommand will wire to the
//! agent loop; here we call the tools directly so you can watch the dispatch.
//!
//! Run: `cargo run -p orcarein-hardware --example sim_arm`
//! Requires `python3`/`python` on PATH (the simulated arm lives in the sidecar).

use std::path::PathBuf;
use std::sync::Arc;

use orcarein_core::ToolRegistry;
use orcarein_hardware::{registry_from_profile, Executor, MockTransport, Profile, Sidecar};
use serde_json::json;
use tokio::sync::Mutex;

/// A device profile for a simulated arm. The `init` block builds a pure-Python
/// stand-in for the hardware (the real `arm.toml` would `import ServoKit`
/// instead); every intent is a single, validated call into it.
const PROFILE: &str = r#"
schema_version = 1

[device]
name = "sim_arm"
description = "Simulated 6-DOF servo arm (pure Python, no hardware)"
transport = "none"

[device.python]
init = """
class _Arm:
    def __init__(self, n):
        self.angles = [90] * n
    def set(self, j, a):
        self.angles[j] = a
    def get(self, j):
        return self.angles[j]
    def home(self):
        for i in range(len(self.angles)):
            self.angles[i] = 90
arm = _Arm(6)
"""

[[intent]]
name = "set_joint"
description = "Set a joint to an absolute angle in degrees."
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
call = "arm.set({joint}, {angle})"

[[intent]]
name = "get_joint"
description = "Read a joint's current angle."
risk = "safe"
backend = "python"
[[intent.param]]
name = "joint"
type = "int"
min = 0
max = 5
[intent.python]
call = "arm.get({joint})"
returns = "int"

[[intent]]
name = "home"
description = "Return all joints to the neutral (90 deg) pose."
risk = "risky"
backend = "python"
[intent.python]
call = "arm.home()"
"#;

#[tokio::main]
async fn main() {
    let Some(py) = Sidecar::locate_python() else {
        eprintln!("no python3/python on PATH — this demo runs the simulated arm in Python");
        return;
    };

    let profile = Profile::from_toml_str(PROFILE).expect("profile parses + validates");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python/sidecar.py");
    let sidecar = Sidecar::spawn(&py, &script, profile.device.python_init.as_deref())
        .await
        .expect("spawn python sidecar");

    let exec = Arc::new(Executor {
        transport: Arc::new(MockTransport::new()),
        sidecar: Some(Mutex::new(sidecar)),
        dry_run: false,
    });
    let registry = registry_from_profile(&profile, exec.clone());

    println!(
        "Loaded profile '{}' -> {} natural-language tools the agent can call:",
        profile.device.name,
        registry.len()
    );
    for name in registry.names() {
        let t = registry.get(name).expect("listed tool resolves");
        println!(
            "   {:<10} [{:?}]  {}",
            t.name(),
            t.risk_level(),
            t.description()
        );
    }

    println!("\n--- live dispatch (each call: validate -> render -> sidecar) ---");
    call(&registry, "set_joint", json!({ "joint": 2, "angle": 150 })).await;
    call(&registry, "get_joint", json!({ "joint": 2 })).await;
    call(&registry, "home", json!({})).await;
    call(&registry, "get_joint", json!({ "joint": 2 })).await;

    println!("\n--- safety: validation rejects bad args BEFORE any actuation ---");
    call(&registry, "set_joint", json!({ "joint": 9, "angle": 150 })).await;

    println!("\n--- safety: --dry-run previews the exact call, executes nothing ---");
    let dry = Arc::new(Executor {
        transport: Arc::new(MockTransport::new()),
        sidecar: None, // dry-run never touches the sidecar
        dry_run: true,
    });
    let dry_registry = registry_from_profile(&profile, dry);
    call(
        &dry_registry,
        "set_joint",
        json!({ "joint": 0, "angle": 45 }),
    )
    .await;
}

/// Look up a tool by name, invoke it like the agent would, and print the result.
async fn call(registry: &ToolRegistry, name: &str, args: serde_json::Value) {
    let tool = registry.get(name).expect("tool exists");
    print!("> {name}({args})\n    -> ");
    match tool.execute(args).await {
        Ok(out) => println!("{}", out.content),
        Err(e) => println!("REJECTED: {e}"),
    }
}
