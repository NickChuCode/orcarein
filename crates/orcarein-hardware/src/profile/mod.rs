// Profile model + TOML parser for OrcaRein hardware device profiles.
// See notes/specs/2026-06-09-orcarein-profile-dsl.md for the DSL spec.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::HardwareError;

mod template;

// ---------------------------------------------------------------------------
// Raw TOML deserialization types (mirrors the TOML schema exactly)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawProfile {
    schema_version: u32,
    device: RawDevice,
    #[serde(rename = "intent", default)]
    intents: Vec<RawIntent>,
}

#[derive(Debug, Deserialize)]
struct RawDevice {
    name: String,
    description: String,
    transport: Transport,
    i2c_bus: Option<u8>,
    i2c_addr: Option<u16>,
    python: Option<RawDevicePython>,
}

#[derive(Debug, Deserialize)]
struct RawDevicePython {
    init: Option<String>,
}

#[derive(Debug, Deserialize)]
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
struct RawNativeBackend {
    op: String,
    #[serde(default)]
    args: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct RawPythonBackend {
    call: String,
    returns: Option<ParamType>,
}

#[derive(Debug, Deserialize)]
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
    pub device: Device,
    pub intents: Vec<Intent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub transport: Transport,
    pub i2c_bus: Option<u8>,
    pub i2c_addr: Option<u16>,
    pub python_init: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    I2c,
    Spi,
    Gpio,
    Uart,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Safe,
    Risky,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    Native {
        op: String,
        args: BTreeMap<String, String>,
    },
    Python {
        call: String,
        returns: Option<ParamType>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub name: String,
    pub description: String,
    pub risk: Risk,
    pub backend: Backend,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: ParamType,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub enum_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Int,
    Float,
    String,
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
    let mut seen_intents: std::collections::HashSet<String> = std::collections::HashSet::new();
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
    let mut seen_params: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        (Option::None, Option::None) => {
            return Err(HardwareError::Validation(format!(
                "intent {:?}: neither [intent.native] nor [intent.python] present; exactly one required",
                name
            )));
        }
        (Some(n), Option::None) => {
            if raw.backend != "native" {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: backend field is {:?} but [intent.native] is present",
                    name, raw.backend
                )));
            }
            // Rule 6: op must be in ALLOWED_OPS
            if !ALLOWED_OPS.contains(&n.op.as_str()) {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: unknown native op {:?}; allowed ops: {:?}",
                    name, n.op, ALLOWED_OPS
                )));
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
        (Option::None, Some(p)) => {
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
    let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();

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
/// Coarse check: reject if `=` appears that is NOT part of `==`, `!=`, `<=`, `>=`.
fn check_not_assignment(intent_name: &str, call: &str) -> Result<(), HardwareError> {
    let bytes = call.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            // Check if it's part of a two-character comparison operator
            let prev = if i > 0 { bytes[i - 1] } else { 0 };
            let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
            let is_comparison = next == b'='   // ==
                || prev == b'!'               // !=
                || prev == b'<'               // <=
                || prev == b'>'; // >=
            if !is_comparison {
                return Err(HardwareError::Validation(format!(
                    "intent {:?}: call has `returns` set but looks like an assignment (contains `=`); \
                     use an expression instead",
                    intent_name
                )));
            }
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
        assert!(Profile::from_toml_str(&dup).is_err());
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let bad = SAMPLE.replace("schema_version = 1", "schema_version = 99");
        assert!(Profile::from_toml_str(&bad).is_err());
    }

    #[test]
    fn rejects_both_backends_present() {
        let bad = SAMPLE.replace(
            "[intent.python]\ncall = \"servo.servo[{joint}].angle = {angle}\"",
            "[intent.python]\ncall = \"x\"\n[intent.native]\nop = \"i2c_scan\"",
        );
        assert!(Profile::from_toml_str(&bad).is_err());
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
        assert!(Profile::from_toml_str(bad).is_err());
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
        assert!(Profile::from_toml_str(&bad).is_err());
    }

    #[test]
    fn rejects_unknown_placeholder_token() {
        let bad = SAMPLE.replace(
            "servo.servo[{joint}].angle = {angle}",
            "servo.servo[{nope}].angle = {angle}",
        );
        assert!(Profile::from_toml_str(&bad).is_err());
    }
}
