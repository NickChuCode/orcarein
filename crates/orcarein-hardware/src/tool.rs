//! `ProfileTool` — wraps one [`Intent`] as an [`orcarein_core::Tool`].
//!
//! [`Executor`] holds the shared transport and optional Python sidecar.
//! [`ProfileTool::new`] binds one intent to one executor; the intent's
//! backend (native or python) is dispatched at `execute` time.
//!
//! # Dry-run mode
//!
//! When `Executor::dry_run` is `true`, `execute` validates arguments normally
//! but returns a description of *what would happen* instead of performing any
//! I/O. This lets callers confirm that an intent + arg set is well-formed
//! without touching hardware or running Python.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use orcarein_core::{RiskLevel, Tool, ToolError, ToolOutput};

use crate::error::HardwareError;
use crate::profile::{validate_and_bind, Backend, Intent, Risk, Scalar};
use crate::transport::{NativeOp, Transport};

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Shared execution backends used by a set of [`ProfileTool`]s built from one
/// device profile.
///
/// Wrap in an `Arc` and clone the arc for each `ProfileTool` built from the
/// same profile so all tools share a single Python namespace.
pub struct Executor {
    /// Native transport (I2C / GPIO / …).  Required even for Python-only
    /// profiles so the type is always present; native intents on a
    /// Python-only profile will simply never be invoked.
    pub transport: Arc<dyn Transport>,
    /// Persistent Python sidecar.  `None` means no Python intents will be
    /// executed for real (dry-run or native-only profiles).
    pub sidecar: Option<Mutex<crate::sidecar::Sidecar>>,
    /// When `true`, `execute` validates arguments and returns a human-readable
    /// description of the operation instead of performing any I/O.
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// ProfileTool
// ---------------------------------------------------------------------------

/// One profile intent exposed as an [`orcarein_core::Tool`].
pub struct ProfileTool {
    intent: Intent,
    exec: Arc<Executor>,
}

impl ProfileTool {
    /// Bind one [`Intent`] to a shared [`Executor`].
    pub fn new(intent: Intent, exec: Arc<Executor>) -> Self {
        Self { intent, exec }
    }
}

// ---------------------------------------------------------------------------
// impl Tool
// ---------------------------------------------------------------------------

#[async_trait]
impl Tool for ProfileTool {
    fn name(&self) -> &str {
        &self.intent.name
    }

    fn description(&self) -> &str {
        &self.intent.description
    }

    fn schema(&self) -> serde_json::Value {
        self.intent.json_schema()
    }

    fn risk_level(&self) -> RiskLevel {
        match self.intent.risk {
            Risk::Safe => RiskLevel::Safe,
            Risk::Risky => RiskLevel::Risky,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        // Step 1: validate + bind arguments (always, even in dry-run).
        let bound = validate_and_bind(&args, &self.intent.params).map_err(to_tool_err)?;

        // Step 2: dispatch to the appropriate backend.
        match &self.intent.backend {
            Backend::Python { call, returns } => {
                let code = crate::profile::render(call, &bound).map_err(to_tool_err)?;

                if self.exec.dry_run {
                    let mode = if returns.is_some() { "eval" } else { "exec" };
                    return Ok(ToolOutput::new(format!(
                        "DRY-RUN: would run python ({mode}): {code}"
                    )));
                }

                let mut sc = self
                    .exec
                    .sidecar
                    .as_ref()
                    .ok_or_else(|| {
                        ToolError::Other(
                            "python backend requires a sidecar but none is configured".into(),
                        )
                    })?
                    .lock()
                    .await;

                if returns.is_some() {
                    let v = sc.eval(code).await.map_err(to_tool_err)?;
                    Ok(ToolOutput::new(v.to_string()))
                } else {
                    sc.exec(code).await.map_err(to_tool_err)?;
                    Ok(ToolOutput::new("ok"))
                }
            }

            Backend::Native {
                op,
                args: native_args,
            } => {
                let native_op = build_native_op(op, native_args, &bound).map_err(to_tool_err)?;

                if self.exec.dry_run {
                    return Ok(ToolOutput::new(format!(
                        "DRY-RUN: would run native: {native_op:?}"
                    )));
                }

                let out = self
                    .exec
                    .transport
                    .exec(native_op)
                    .await
                    .map_err(to_tool_err)?;

                Ok(ToolOutput::new(out))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Error converter
// ---------------------------------------------------------------------------

fn to_tool_err(e: HardwareError) -> ToolError {
    ToolError::Other(e.to_string())
}

// ---------------------------------------------------------------------------
// build_native_op — construct a NativeOp from profile args + bound params
// ---------------------------------------------------------------------------

/// Construct a [`NativeOp`] from the profile's `[intent.native]` `op` and
/// `args`, resolving `{param}` placeholders against `bound`.
///
/// Each native arg value is either:
/// - A single `{name}` placeholder → look up `bound[name]` and coerce.
/// - A literal (e.g. `"0x3d"`, `"true"`) → parse directly.
fn build_native_op(
    op: &str,
    native_args: &BTreeMap<String, String>,
    bound: &BTreeMap<String, Scalar>,
) -> Result<NativeOp, HardwareError> {
    match op {
        "i2c_scan" => Ok(NativeOp::I2cScan),

        "i2c_write_reg" => {
            let reg = resolve_int(
                native_args.get("reg").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let value = resolve_int(
                native_args.get("value").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let reg_u8 = to_u8(reg, "reg")?;
            let value_u8 = to_u8(value, "value")?;
            Ok(NativeOp::I2cWriteReg {
                reg: reg_u8,
                value: value_u8,
            })
        }

        "i2c_read_reg" => {
            let reg = resolve_int(
                native_args.get("reg").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let reg_u8 = to_u8(reg, "reg")?;
            Ok(NativeOp::I2cReadReg { reg: reg_u8 })
        }

        "gpio_set" => {
            let pin = resolve_int(
                native_args.get("pin").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let high = resolve_bool(
                native_args.get("high").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let pin_u8 = to_u8(pin, "pin")?;
            Ok(NativeOp::GpioSet { pin: pin_u8, high })
        }

        "gpio_read" => {
            let pin = resolve_int(
                native_args.get("pin").map(String::as_str).unwrap_or(""),
                bound,
            )?;
            let pin_u8 = to_u8(pin, "pin")?;
            Ok(NativeOp::GpioRead { pin: pin_u8 })
        }

        unknown => Err(HardwareError::Validation(format!(
            "unknown native op {:?}; this is a bug — the profile parser should have rejected it",
            unknown
        ))),
    }
}

/// Resolve an arg template to an `i64`.
///
/// If `template` is exactly `{name}`, look up `bound[name]` and require it
/// is a `Scalar::Int`. Otherwise parse the literal — supports `0x`/`0X`
/// hexadecimal and plain decimal.
fn resolve_int(template: &str, bound: &BTreeMap<String, Scalar>) -> Result<i64, HardwareError> {
    if let Some(name) = single_placeholder(template) {
        match bound.get(name) {
            Some(Scalar::Int(n)) => Ok(*n),
            Some(other) => Err(HardwareError::Validation(format!(
                "native arg {{{name}}}: expected int, got {other:?}"
            ))),
            None => Err(HardwareError::Validation(format!(
                "native arg {{{name}}}: not in bound params"
            ))),
        }
    } else {
        // Literal — try hex then decimal.
        let s = template.trim();
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).map_err(|_| {
                HardwareError::Validation(format!("native arg: cannot parse {:?} as hex int", s))
            })
        } else {
            s.parse::<i64>().map_err(|_| {
                HardwareError::Validation(format!("native arg: cannot parse {:?} as int", s))
            })
        }
    }
}

/// Resolve an arg template to a `bool`.
///
/// If `template` is exactly `{name}`, look up `bound[name]` and require it
/// is a `Scalar::Bool`. Otherwise parse the literal `"true"`/`"false"`.
fn resolve_bool(template: &str, bound: &BTreeMap<String, Scalar>) -> Result<bool, HardwareError> {
    if let Some(name) = single_placeholder(template) {
        match bound.get(name) {
            Some(Scalar::Bool(b)) => Ok(*b),
            Some(other) => Err(HardwareError::Validation(format!(
                "native arg {{{name}}}: expected bool, got {other:?}"
            ))),
            None => Err(HardwareError::Validation(format!(
                "native arg {{{name}}}: not in bound params"
            ))),
        }
    } else {
        match template.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(HardwareError::Validation(format!(
                "native arg: cannot parse {:?} as bool; expected \"true\" or \"false\"",
                other
            ))),
        }
    }
}

/// If `template` is exactly the form `{name}` (one placeholder, no other
/// text), return `Some(name)`. Otherwise return `None`.
fn single_placeholder(template: &str) -> Option<&str> {
    let t = template.trim();
    if t.starts_with('{') && t.ends_with('}') && t.len() > 2 {
        let inner = &t[1..t.len() - 1];
        // Must be a valid identifier (no spaces, no nested braces).
        if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Some(inner);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// registry_from_profile
// ---------------------------------------------------------------------------

/// Build a [`orcarein_core::ToolRegistry`] from a device profile: each
/// intent in the profile becomes a [`ProfileTool`] sharing the given
/// [`Executor`].
///
/// Tools are inserted in profile order; the registry's internal `BTreeMap`
/// stores them in sorted order so [`orcarein_core::ToolRegistry::names`]
/// returns names alphabetically.
pub fn registry_from_profile(
    profile: &crate::profile::Profile,
    exec: Arc<Executor>,
) -> orcarein_core::ToolRegistry {
    let mut registry = orcarein_core::ToolRegistry::new();
    for intent in &profile.intents {
        registry.register(Box::new(ProfileTool::new(intent.clone(), exec.clone())));
    }
    registry
}

/// Range-check an `i64` and cast to `u8`; error on out-of-range.
fn to_u8(v: i64, field: &str) -> Result<u8, HardwareError> {
    if !(0..=255).contains(&v) {
        return Err(HardwareError::Validation(format!(
            "native arg {field}: value {v} is out of u8 range (0..=255)"
        )));
    }
    Ok(v as u8)
}
