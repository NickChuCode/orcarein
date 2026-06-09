// Profile model + TOML parser for OrcaRein hardware device profiles.
// See notes/specs/2026-06-09-orcarein-profile-dsl.md for the DSL spec.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use crate::error::HardwareError;

mod schema;
mod template;

// Task 6 will call these; suppress dead_code until then.
#[allow(unused_imports)]
pub(crate) use template::{render, validate_and_bind, Scalar};

// ---------------------------------------------------------------------------
// Raw TOML deserialization types (mirrors the TOML schema exactly)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    schema_version: u32,
    device: RawDevice,
    #[serde(rename = "intent", default)]
    intents: Vec<RawIntent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    name: String,
    description: String,
    transport: Transport,
    i2c_bus: Option<u8>,
    i2c_addr: Option<u16>,
    python: Option<RawDevicePython>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevicePython {
    init: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIntent {
    name: String,
    description: String,
    risk: Risk,
    backend: String,
    #[serde(rename = "param", default)]
    params: Vec<RawParam>,
    native: Option<RawNativeBackend>,
    python: Option<RawPythonBackend>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNativeBackend {
    op: String,
    #[serde(default)]
    args: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPythonBackend {
    call: String,
    returns: Option<ParamType>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    name: String,
    #[serde(rename = "type")]
    ty: ParamType,
    min: Option<f64>,
    max: Option<f64>,
    #[serde(rename = "enum")]
    enum_values: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Public validated types
// ---------------------------------------------------------------------------

/// A fully-parsed and validated device profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The device this profile describes (transport + connection details).
    pub device: Device,
    /// The intents (named operations) the device exposes.
    pub intents: Vec<Intent>,
}

/// A hardware device: how to reach it and how to bring it up.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// Human-readable device name.
    pub name: String,
    /// Free-text description of the device.
    pub description: String,
    /// The bus/transport used to talk to the device.
    pub transport: Transport,
    /// I2C bus number (only meaningful for `Transport::I2c`).
    pub i2c_bus: Option<u8>,
    /// I2C device address (only meaningful for `Transport::I2c`).
    pub i2c_addr: Option<u16>,
    /// Python setup snippet run once before any Python-backed intent, e.g.
    /// importing a driver and binding it to a name the calls reference.
    pub python_init: Option<String>,
}

/// The bus or protocol used to communicate with a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// I2C bus.
    I2c,
    /// SPI bus.
    Spi,
    /// GPIO pins.
    Gpio,
    /// UART/serial.
    Uart,
    /// No physical bus (e.g. a pure-Python virtual device).
    None,
}

/// How dangerous an intent is, used to gate execution behind confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Read-only or otherwise harmless; may run without confirmation.
    Safe,
    /// Mutates device state; should require confirmation before running.
    Risky,
}

/// The execution backend for an intent: exactly one is present per intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// A built-in native operation (one of the allowed native ops).
    Native {
        /// The native op name (validated against the allowed-ops list).
        op: String,
        /// Substitutable command args: values may contain `{param}`
        /// placeholders filled in from the intent's params at call time.
        args: BTreeMap<String, String>,
    },
    /// A Python expression or statement evaluated against `python_init`.
    Python {
        /// The Python call template; `{param}` placeholders are substituted
        /// from the intent's params at call time.
        call: String,
        /// If set, `call` is an expression whose result is coerced to this
        /// type and returned; if `None`, `call` is run for its side effects.
        returns: Option<ParamType>,
    },
}

/// A named operation a device can perform, with its params and backend.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    /// Unique (within the profile) intent name.
    pub name: String,
    /// Free-text description of what the intent does.
    pub description: String,
    /// How dangerous the intent is.
    pub risk: Risk,
    /// The backend that executes the intent.
    pub backend: Backend,
    /// The parameters the intent accepts.
    pub params: Vec<Param>,
}

/// A typed parameter accepted by an intent.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Unique (within the intent) parameter name.
    pub name: String,
    /// The parameter's value type.
    pub ty: ParamType,
    /// Inclusive lower bound (int/float params only).
    pub min: Option<f64>,
    /// Inclusive upper bound (int/float params only).
    pub max: Option<f64>,
    /// Allowed string values (string params only); required when a string
    /// param is referenced from a Python call, to prevent injection.
    pub enum_values: Option<Vec<String>>,
}

/// The value type of a parameter (or a Python `returns` coercion target).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    /// Integer.
    Int,
    /// Floating-point number.
    Float,
    /// String.
    String,
    /// Boolean.
    Bool,
}

// ---------------------------------------------------------------------------
// Allowed native ops (§1.4)
// ---------------------------------------------------------------------------

const ALLOWED_OPS: &[&str] = &[
    "i2c_scan",
    "i2c_write_reg",
    "i2c_read_reg",
    "gpio_set",
    "gpio_read",
];

/// Required `args` keys for each native op (§1.4). The `args` keys of a profile
/// must exactly match this set (no missing, no extra) for the op to be valid.
fn required_native_args(op: &str) -> Option<&'static [&'static str]> {
    match op {
        "i2c_scan" => Some(&[]),
        "i2c_write_reg" => Some(&["reg", "value"]),
        "i2c_read_reg" => Some(&["reg"]),
        "gpio_set" => Some(&["pin", "high"]),
        "gpio_read" => Some(&["pin"]),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Profile::from_toml_str entry point
// ---------------------------------------------------------------------------

impl Profile {
    /// Parse and validate a TOML device profile string.
    /// Returns `HardwareError::Parse` for TOML syntax errors,
    /// `HardwareError::Validation` for semantic rule violations.
    pub fn from_toml_str(s: &str) -> Result<Profile, HardwareError> {
        let raw: RawProfile = toml::from_str(s).map_err(|e| HardwareError::Parse(e.to_string()))?;
        validate_and_convert(raw)
    }
}

// ---------------------------------------------------------------------------
// Conversion + validation
// ---------------------------------------------------------------------------

fn validate_and_convert(raw: RawProfile) -> Result<Profile, HardwareError> {
    // Rule 1: schema_version == 1
    if raw.schema_version != 1 {
        return Err(HardwareError::Validation(format!(
            "unsupported schema_version {}; only version 1 is supported",
            raw.schema_version
        )));
    }

    // Convert device
    let device = Device {
        name: raw.device.name,
        description: raw.device.description,
        transport: raw.device.transport,
        i2c_bus: raw.device.i2c_bus,
        i2c_addr: raw.device.i2c_addr,
        python_init: raw.device.python.and_then(|p| p.init),
    };

    // Rule 2: intent names must be unique
    let mut seen_intents: HashSet<String> = HashSet::new();
    let mut intents = Vec::with_capacity(raw.intents.len());
    for raw_intent in raw.intents {
        if !seen_intents.insert(raw_intent.name.clone()) {
            return Err(HardwareError::Validation(format!(
                "duplicate intent name {:?}",
                raw_intent.name
            )));
        }
        let intent = convert_intent(raw_intent)?;
        intents.push(intent);
    }

    Ok(Profile { device, intents })
}

fn convert_intent(raw: RawIntent) -> Result<Intent, HardwareError> {
    let name = raw.name;

    // Rule 2: param names unique within intent
    let mut seen_params: HashSet<String> = HashSet::new();
    let mut params: Vec<Param> = Vec::with_capacity(raw.params.len());
    for rp in raw.params {
        if !seen_params.insert(rp.name.clone()) {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: duplicate param name {:?}",
                name, rp.name
            )));
        }

        // Rule 4: min/max only on int/float; enum only on string; min <= max
        match rp.ty {
            ParamType::Int | ParamType::Float => {
                if rp.enum_values.is_some() {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}, param {:?}: `enum` is only valid for string params",
                        name, rp.name
                    )));
                }
                if let (Some(mn), Some(mx)) = (rp.min, rp.max) {
                    if mn > mx {
                        return Err(HardwareError::Validation(format!(
                            "intent {:?}, param {:?}: min ({}) > max ({})",
                            name, rp.name, mn, mx
                        )));
                    }
                }
            }
            ParamType::String | ParamType::Bool => {
                if rp.min.is_some() || rp.max.is_some() {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}, param {:?}: `min`/`max` only valid for int/float params",
                        name, rp.name
                    )));
                }
            }
        }

        // Rule 7 (partial): validate enum values match pattern
        if let Some(ref ev) = rp.enum_values {
            for val in ev {
                if !is_valid_enum_member(val) {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}, param {:?}: enum member {:?} does not match [A-Za-z0-9_.-]+",
                        name, rp.name, val
                    )));
                }
            }
        }

        params.push(Param {
            name: rp.name,
            ty: rp.ty,
            min: rp.min,
            max: rp.max,
            enum_values: rp.enum_values,
        });
    }

    // Rule 3: exactly one of native/python
    let backend = match (raw.native, raw.python) {
        (Some(_), Some(_)) => {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: both [intent.native] and [intent.python] present; exactly one required",
                name
            )));
        }
        (None, None) => {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: neither [intent.native] nor [intent.python] present; exactly one required",
                name
            )));
        }
        (Some(n), None) => {
            if raw.backend != "native" {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: backend field is {:?} but [intent.native] is present",
                    name, raw.backend
                )));
            }
            // Rule 6 (first clause): op must be in ALLOWED_OPS
            let required = match required_native_args(&n.op) {
                Some(r) => r,
                None => {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}: unknown native op {:?}; allowed ops: {:?}",
                        name, n.op, ALLOWED_OPS
                    )));
                }
            };
            // Rule 6 (second clause): args keys must exactly match the op's
            // required named params (no missing, no extra).
            for key in n.args.keys() {
                if !required.contains(&key.as_str()) {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}: native op {:?} got unexpected arg {:?}; required args: {:?}",
                        name, n.op, key, required
                    )));
                }
            }
            for req in required {
                if !n.args.contains_key(*req) {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}: native op {:?} missing required arg {:?}; required args: {:?}",
                        name, n.op, req, required
                    )));
                }
            }
            // Convert args values to strings
            let mut args: BTreeMap<String, String> = BTreeMap::new();
            for (k, v) in n.args {
                let s = toml_value_to_string(&v);
                args.insert(k, s);
            }
            // Rule 5 for native: validate placeholder references in args
            let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
            validate_templates_native(&name, &args, &param_names)?;
            Backend::Native { op: n.op, args }
        }
        (None, Some(p)) => {
            if raw.backend != "python" {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: backend field is {:?} but [intent.python] is present",
                    name, raw.backend
                )));
            }

            // Rule 8: if returns is set, call must be an expression (no top-level assignment)
            if p.returns.is_some() {
                check_not_assignment(&name, &p.call)?;
            }

            // Rule 5 + 7 for python: validate placeholder references in call
            let param_names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
            validate_python_call(&name, &p.call, &params, &param_names)?;

            Backend::Python {
                call: p.call,
                returns: p.returns,
            }
        }
    };

    Ok(Intent {
        name,
        description: raw.description,
        risk: raw.risk,
        backend,
        params,
    })
}

// ---------------------------------------------------------------------------
// Template validation helpers
// ---------------------------------------------------------------------------

/// For native backend: every {name} in args values must be a declared param,
/// and every param must appear in at least one arg value.
fn validate_templates_native(
    intent_name: &str,
    args: &BTreeMap<String, String>,
    param_names: &[&str],
) -> Result<(), HardwareError> {
    let mut referenced: HashSet<String> = HashSet::new();

    for val in args.values() {
        let tokens = template::placeholders(val)?;
        for token in tokens {
            if !param_names.contains(&token.as_str()) {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: template references unknown param {:?}",
                    intent_name, token
                )));
            }
            referenced.insert(token);
        }
    }

    // Every param must be referenced
    for pn in param_names {
        if !referenced.contains(*pn) {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: param {:?} is declared but not referenced in any template",
                intent_name, pn
            )));
        }
    }

    Ok(())
}

/// For python backend: validates the call template per rules 5 and 7.
fn validate_python_call(
    intent_name: &str,
    call: &str,
    params: &[Param],
    param_names: &[&str],
) -> Result<(), HardwareError> {
    let tokens = template::placeholders(call)?;

    // Rule 5: every {name} must be a declared param
    for token in &tokens {
        if !param_names.contains(&token.as_str()) {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: call template references unknown param {:?}",
                intent_name, token
            )));
        }
    }

    // Rule 5: every declared param must be referenced at least once
    for pn in param_names {
        if !tokens.contains(&pn.to_string()) {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: param {:?} is declared but not referenced in call template",
                intent_name, pn
            )));
        }
    }

    // Rule 7: for each referenced token, if its type is string, it must have enum
    for token in &tokens {
        if let Some(param) = params.iter().find(|p| p.name == *token) {
            if param.ty == ParamType::String && param.enum_values.is_none() {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: string param {:?} used in python call but has no `enum` whitelist (injection risk)",
                    intent_name, token
                )));
            }
        }
    }

    Ok(())
}

/// Rule 8: a call with `returns` must be an expression, not an assignment.
///
/// An *assignment* is a top-level `=` — i.e. at paren/bracket/brace nesting
/// depth 0 — that is not part of a comparison operator (`==`, `!=`, `<=`,
/// `>=`). We track nesting depth while scanning so that keyword arguments
/// (`read_reg(channel=3)`, whose `=` sits inside `(...)` at depth 1) and
/// comparisons are allowed, while a real `x = 1` is rejected.
fn check_not_assignment(intent_name: &str, call: &str) -> Result<(), HardwareError> {
    let bytes = call.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'=' => {
                let prev = if i > 0 { bytes[i - 1] } else { 0 };
                let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
                // Part of a two-character comparison operator?
                let is_comparison = next == b'='   // first `=` of `==`
                    || prev == b'='               // second `=` of `==`
                    || prev == b'!'               // `!=`
                    || prev == b'<'               // `<=`
                    || prev == b'>'; // `>=`
                if depth == 0 && !is_comparison {
                    return Err(HardwareError::Validation(format!(
                        "intent {:?}: call has `returns` set but looks like an assignment \
                         (top-level `=`); use an expression instead",
                        intent_name
                    )));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// Convert a TOML value to a string for template substitution.
fn toml_value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Check that an enum member matches `[A-Za-z0-9_.-]+`.
fn is_valid_enum_member(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

// ---------------------------------------------------------------------------
// Tests (Step A — written first, before implementation)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema_version = 1
[device]
name = "arm"
description = "test arm"
transport = "i2c"
i2c_bus = 1
i2c_addr = 0x40

[[intent]]
name = "i2c_scan"
description = "scan the bus"
risk = "safe"
backend = "native"
[intent.native]
op = "i2c_scan"

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

    #[test]
    fn parses_sample_profile() {
        let p = Profile::from_toml_str(SAMPLE).expect("valid profile");
        assert_eq!(p.device.name, "arm");
        assert_eq!(p.device.transport, Transport::I2c);
        assert_eq!(p.device.i2c_addr, Some(0x40));
        assert_eq!(p.intents.len(), 2);
        assert_eq!(p.intents[0].name, "i2c_scan");
        assert_eq!(p.intents[0].risk, Risk::Safe);
        assert!(matches!(p.intents[0].backend, Backend::Native { .. }));
        assert_eq!(p.intents[1].params.len(), 2);
        assert_eq!(p.intents[1].params[1].max, Some(180.0));
        assert!(matches!(p.intents[1].backend, Backend::Python { .. }));
    }

    #[test]
    fn rejects_duplicate_intent_names() {
        let dup = SAMPLE.replace("set_joint", "i2c_scan");
        let err = Profile::from_toml_str(&dup).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let bad = SAMPLE.replace("schema_version = 1", "schema_version = 99");
        let err = Profile::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_both_backends_present() {
        let bad = SAMPLE.replace(
            "[intent.python]\ncall = \"servo.servo[{joint}].angle = {angle}\"",
            "[intent.python]\ncall = \"x\"\n[intent.native]\nop = \"i2c_scan\"",
        );
        let err = Profile::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_free_string_param_in_python_call() {
        // §2.5 rule 7: a string param referenced by a python call MUST have enum.
        let bad = r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "none"
[[intent]]
name = "do"
description = "x"
risk = "risky"
backend = "python"
[[intent.param]]
name = "mode"
type = "string"
[intent.python]
call = "set_mode({mode})"
"#;
        let err = Profile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn accepts_enum_string_param_in_python_call() {
        let ok = r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "none"
[[intent]]
name = "do"
description = "x"
risk = "risky"
backend = "python"
[[intent.param]]
name = "mode"
type = "string"
enum = ["a", "b"]
[intent.python]
call = "set_mode(\"{mode}\")"
"#;
        assert!(Profile::from_toml_str(ok).is_ok());
    }

    #[test]
    fn rejects_min_greater_than_max() {
        let bad = SAMPLE.replace("min = 0\nmax = 5", "min = 5\nmax = 0");
        let err = Profile::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_unknown_placeholder_token() {
        let bad = SAMPLE.replace(
            "servo.servo[{joint}].angle = {angle}",
            "servo.servo[{nope}].angle = {angle}",
        );
        let err = Profile::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    // Fix 1: Rule 6 second clause — native args keys must exactly match the
    // op's required named params.
    #[test]
    fn rejects_native_op_with_missing_required_arg() {
        // i2c_write_reg requires {reg, value}; supplying only {reg} is invalid.
        let bad = r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "i2c"
i2c_bus = 1
[[intent]]
name = "wr"
description = "x"
risk = "risky"
backend = "native"
[[intent.param]]
name = "reg"
type = "int"
[intent.native]
op = "i2c_write_reg"
args = { reg = "{reg}" }
"#;
        let err = Profile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn accepts_native_op_with_correct_args() {
        // i2c_write_reg with exactly {reg, value} is accepted.
        let ok = r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "i2c"
i2c_bus = 1
[[intent]]
name = "wr"
description = "x"
risk = "risky"
backend = "native"
[[intent.param]]
name = "reg"
type = "int"
[[intent.param]]
name = "value"
type = "int"
[intent.native]
op = "i2c_write_reg"
args = { reg = "{reg}", value = "{value}" }
"#;
        let p = Profile::from_toml_str(ok).expect("valid i2c_write_reg profile");
        assert!(matches!(
            p.intents[0].backend,
            Backend::Native { ref op, .. } if op == "i2c_write_reg"
        ));
    }

    #[test]
    fn rejects_native_op_with_extra_arg() {
        // i2c_read_reg requires only {reg}; an extra key is rejected.
        let bad = r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "i2c"
i2c_bus = 1
[[intent]]
name = "rd"
description = "x"
risk = "safe"
backend = "native"
[[intent.param]]
name = "reg"
type = "int"
[intent.native]
op = "i2c_read_reg"
args = { reg = "{reg}", bogus = "1" }
"#;
        let err = Profile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    // Fix 3: regression test for the corrected flagship example —
    // `returns` lives INSIDE [intent.python], not at [[intent]] top level.
    #[test]
    fn parses_python_returns_inside_intent_python() {
        let toml = r#"
schema_version = 1
[device]
name = "arm"
description = "x"
transport = "i2c"
i2c_bus = 1
[[intent]]
name = "read_angle"
description = "read"
risk = "safe"
backend = "python"
[[intent.param]]
name = "joint"
type = "int"
min = 0
max = 5
[intent.python]
call = "servo.servo[{joint}].angle"
returns = "float"
"#;
        let p = Profile::from_toml_str(toml).expect("valid python returns profile");
        assert!(matches!(
            p.intents[0].backend,
            Backend::Python {
                returns: Some(ParamType::Float),
                ..
            }
        ));
    }

    /// Build a python intent whose call has `returns` set, for exercising
    /// the assignment check. The `name` param is declared so the call's
    /// `{name}` placeholder resolves; `enum` makes it usable in a call.
    fn python_returns_profile(call: &str) -> String {
        format!(
            r#"
schema_version = 1
[device]
name = "d"
description = "x"
transport = "none"
[[intent]]
name = "do"
description = "x"
risk = "safe"
backend = "python"
[[intent.param]]
name = "name"
type = "string"
enum = ["chan0", "chan1"]
[intent.python]
call = "{call}"
returns = "int"
"#
        )
    }

    // Fix 1: check_not_assignment must allow Python keyword arguments and
    // comparisons, only rejecting a true top-level (depth-0) assignment.
    #[test]
    fn returns_allows_kwarg_call() {
        // The `=` is inside `(...)` → depth 1 → a kwarg, not an assignment.
        let toml = python_returns_profile("read_reg(channel={name})");
        assert!(
            Profile::from_toml_str(&toml).is_ok(),
            "kwarg call with returns should parse"
        );
    }

    #[test]
    fn returns_allows_comparison_call() {
        // `==` is a comparison operator, not an assignment.
        let toml = python_returns_profile("servo.connected[{name}] == True");
        assert!(
            Profile::from_toml_str(&toml).is_ok(),
            "comparison call with returns should parse"
        );
    }

    #[test]
    fn returns_allows_plain_expression_call() {
        // No `=` at all → a plain attribute/index expression.
        let toml = python_returns_profile("servo.servo[{name}].angle");
        assert!(
            Profile::from_toml_str(&toml).is_ok(),
            "plain expression call with returns should parse"
        );
    }

    #[test]
    fn returns_rejects_top_level_assignment() {
        // A depth-0 `=` is a real assignment and must be rejected.
        let toml = python_returns_profile("x[{name}] = 1");
        let err = Profile::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_returns_at_intent_top_level() {
        // `returns` at the [[intent]] top level is not a field of RawIntent,
        // so deny_unknown_fields must reject it as a Parse error.
        let bad = r#"
schema_version = 1
[device]
name = "arm"
description = "x"
transport = "i2c"
i2c_bus = 1
[[intent]]
name = "read_angle"
description = "read"
risk = "safe"
backend = "python"
returns = "float"
[[intent.param]]
name = "joint"
type = "int"
min = 0
max = 5
[intent.python]
call = "servo.servo[{joint}].angle"
"#;
        let err = Profile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Parse(_)), "{err:?}");
    }
}
