//! Placeholder scanning shared by profile validation (Task 2) and the runtime
//! renderer (Task 3).
//!
//! Three things live here:
//! 1. `placeholders` — brace-scanning used during profile validation.
//! 2. `Scalar` — a typed runtime value produced by `validate_and_bind`.
//! 3. `validate_and_bind` + `render` — the agent-arg → rendered-call pipeline.
use std::collections::BTreeMap;

use crate::error::HardwareError;
use crate::profile::{Param, ParamType};

// ---------------------------------------------------------------------------
// Scalar — a typed runtime value
// ---------------------------------------------------------------------------

/// A typed value bound from a JSON argument object.
///
/// Produced by [`validate_and_bind`] and consumed by [`render`].
/// `Display` emits the Python-literal form of the value (see field docs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Scalar {
    /// An integer value. Displays as its decimal representation (e.g. `90`).
    Int(i64),
    /// A floating-point value. Displays as Rust's default `f64` formatting.
    Float(f64),
    /// A string value. Displays **without** surrounding quotes — the template
    /// author is responsible for quoting if Python requires it.
    Str(String),
    /// A boolean value. Displays as Python literals: `True` or `False`.
    Bool(bool),
}

impl std::fmt::Display for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scalar::Int(n) => write!(f, "{n}"),
            Scalar::Float(v) => write!(f, "{v}"),
            Scalar::Str(s) => write!(f, "{s}"),
            Scalar::Bool(true) => write!(f, "True"),
            Scalar::Bool(false) => write!(f, "False"),
        }
    }
}

// ---------------------------------------------------------------------------
// validate_and_bind
// ---------------------------------------------------------------------------

/// Validate a JSON argument object against a slice of declared `Param`s and
/// produce a `BTreeMap<name, Scalar>` ready for [`render`].
///
/// Rules enforced:
/// - `args` must be a JSON object.
/// - Every declared param must be present in `args` (all params are required).
/// - Every key in `args` must correspond to a declared param (strict — no
///   extra keys).
/// - Each value must be type-compatible with its `ParamType`:
///   - `Int` ← JSON integer
///   - `Float` ← JSON number (integers are accepted as floats)
///   - `String` ← JSON string
///   - `Bool` ← JSON boolean
/// - For `Int`/`Float` params with `min`/`max`, the value must be in range
///   (inclusive).
/// - For `String` params with `enum_values`, the value must be a member.
///
/// Returns `HardwareError::Validation` with an actionable message on any
/// violation.
pub(crate) fn validate_and_bind(
    args: &serde_json::Value,
    params: &[Param],
) -> Result<BTreeMap<String, Scalar>, HardwareError> {
    let obj = args
        .as_object()
        .ok_or_else(|| HardwareError::Validation("args must be a JSON object".to_string()))?;

    // Check for unexpected keys first (strict binding)
    let param_names: std::collections::HashSet<&str> =
        params.iter().map(|p| p.name.as_str()).collect();
    for key in obj.keys() {
        if !param_names.contains(key.as_str()) {
            return Err(HardwareError::Validation(format!(
                "unexpected argument {:?}; declared params: {:?}",
                key,
                params.iter().map(|p| &p.name).collect::<Vec<_>>()
            )));
        }
    }

    let mut bound: BTreeMap<String, Scalar> = BTreeMap::new();

    for param in params {
        let val = obj.get(&param.name).ok_or_else(|| {
            HardwareError::Validation(format!("missing required argument {:?}", param.name))
        })?;

        let scalar = bind_param(param, val)?;
        bound.insert(param.name.clone(), scalar);
    }

    Ok(bound)
}

/// Bind a single JSON value to a `Scalar` for one `Param`, validating type and
/// range/enum constraints.
fn bind_param(param: &Param, val: &serde_json::Value) -> Result<Scalar, HardwareError> {
    match param.ty {
        ParamType::Int => {
            let n = val.as_i64().ok_or_else(|| {
                HardwareError::Validation(format!(
                    "param {:?}: expected integer, got {:?}",
                    param.name, val
                ))
            })?;
            if let Some(min) = param.min {
                if (n as f64) < min {
                    return Err(HardwareError::Validation(format!(
                        "param {:?}: value {n} is below minimum {min}",
                        param.name
                    )));
                }
            }
            if let Some(max) = param.max {
                if (n as f64) > max {
                    return Err(HardwareError::Validation(format!(
                        "param {:?}: value {n} is above maximum {max}",
                        param.name
                    )));
                }
            }
            Ok(Scalar::Int(n))
        }
        ParamType::Float => {
            // Accept both JSON integers and JSON floats.
            let v = val.as_f64().ok_or_else(|| {
                HardwareError::Validation(format!(
                    "param {:?}: expected number, got {:?}",
                    param.name, val
                ))
            })?;
            if let Some(min) = param.min {
                if v < min {
                    return Err(HardwareError::Validation(format!(
                        "param {:?}: value {v} is below minimum {min}",
                        param.name
                    )));
                }
            }
            if let Some(max) = param.max {
                if v > max {
                    return Err(HardwareError::Validation(format!(
                        "param {:?}: value {v} is above maximum {max}",
                        param.name
                    )));
                }
            }
            Ok(Scalar::Float(v))
        }
        ParamType::String => {
            let s = val.as_str().ok_or_else(|| {
                HardwareError::Validation(format!(
                    "param {:?}: expected string, got {:?}",
                    param.name, val
                ))
            })?;
            if let Some(ref ev) = param.enum_values {
                if !ev.iter().any(|e| e == s) {
                    return Err(HardwareError::Validation(format!(
                        "param {:?}: value {:?} not in allowed enum {:?}",
                        param.name, s, ev
                    )));
                }
            }
            Ok(Scalar::Str(s.to_string()))
        }
        ParamType::Bool => {
            let b = val.as_bool().ok_or_else(|| {
                HardwareError::Validation(format!(
                    "param {:?}: expected boolean, got {:?}",
                    param.name, val
                ))
            })?;
            Ok(Scalar::Bool(b))
        }
    }
}

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

/// Substitute `{name}` placeholders in `template` with values from `bound`,
/// returning the fully-rendered string.
///
/// Uses [`placeholders`] for token discovery so the brace grammar is
/// consistent with profile validation. Literal text outside placeholders is
/// passed through unchanged.
///
/// Returns `HardwareError::Validation` if any `{name}` in the template has no
/// entry in `bound` (should not occur after a successful `validate_and_bind`,
/// but is defended against explicitly).
pub(crate) fn render(
    template: &str,
    bound: &BTreeMap<String, Scalar>,
) -> Result<String, HardwareError> {
    // We need to rebuild the string character-by-character, replacing
    // each {name} with its bound value.  We re-use the same brace grammar
    // as `placeholders` so the two are always in sync.
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;

    while i < len {
        match chars[i] {
            '{' => {
                // Consume the opening brace, read the ident, consume closing brace.
                // `placeholders` already verified syntax during profile load, but
                // we still defend so `render` can be called on arbitrary templates.
                i += 1;
                let start = i;
                if i >= len {
                    return Err(HardwareError::Validation(
                        "unmatched `{` at end of template".to_string(),
                    ));
                }
                while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                if i >= len || chars[i] != '}' {
                    let ident: String = chars[start..i].iter().collect();
                    return Err(HardwareError::Validation(format!(
                        "invalid placeholder `{{{ident}` — expected `}}` to close"
                    )));
                }
                let ident: String = chars[start..i].iter().collect();
                i += 1; // consume '}'
                let scalar = bound.get(&ident).ok_or_else(|| {
                    HardwareError::Validation(format!(
                        "template references unbound name {:?}",
                        ident
                    ))
                })?;
                out.push_str(&scalar.to_string());
            }
            '}' => {
                return Err(HardwareError::Validation(
                    "unmatched `}` in template".to_string(),
                ));
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }

    Ok(out)
}

/// Extract the `{name}` placeholder names from a template, in order.
/// Errors on a malformed brace: a `{` that does not open a valid `{ident}`
/// token, or an unmatched `{`/`}`. This is the non-Turing-complete enforcement
/// point — only `{ident}` tokens and literal text are allowed.
pub(crate) fn placeholders(template: &str) -> Result<Vec<String>, HardwareError> {
    let mut result = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '{' => {
                // Start of a placeholder — must be followed by a valid ident then '}'
                i += 1;
                let start = i;
                // Read identifier: [A-Za-z_][A-Za-z0-9_]*
                if i >= len {
                    return Err(HardwareError::Validation(
                        "unmatched `{` at end of template".to_string(),
                    ));
                }
                // First char must be alphabetic or underscore
                if !chars[i].is_ascii_alphabetic() && chars[i] != '_' {
                    return Err(HardwareError::Validation(format!(
                        "invalid placeholder: `{{{}` — identifier must start with [A-Za-z_]",
                        chars[i]
                    )));
                }
                // Read rest of identifier
                while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                if i >= len || chars[i] != '}' {
                    return Err(HardwareError::Validation(format!(
                        "invalid placeholder: `{{{}` — expected `}}` to close identifier",
                        chars[start..i].iter().collect::<String>()
                    )));
                }
                let ident: String = chars[start..i].iter().collect();
                if ident.is_empty() {
                    return Err(HardwareError::Validation(
                        "empty placeholder `{}` is not allowed".to_string(),
                    ));
                }
                result.push(ident);
                i += 1; // consume '}'
            }
            '}' => {
                return Err(HardwareError::Validation(
                    "unmatched `}` in template".to_string(),
                ));
            }
            _ => {
                i += 1;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // --- existing placeholder tests ---

    #[test]
    fn extracts_in_order() {
        assert_eq!(
            placeholders("a{x}b{y}").unwrap(),
            vec!["x".to_string(), "y".to_string()]
        );
    }
    #[test]
    fn no_tokens_ok() {
        assert!(placeholders("home()").unwrap().is_empty());
    }
    #[test]
    fn rejects_empty_braces() {
        assert!(placeholders("a{}b").is_err());
    }
    #[test]
    fn rejects_unmatched() {
        assert!(placeholders("a{x").is_err());
    }

    // --- Scalar Display ---

    #[test]
    fn scalar_display_int() {
        assert_eq!(Scalar::Int(90).to_string(), "90");
    }

    #[test]
    fn scalar_display_float() {
        assert_eq!(Scalar::Float(1.5).to_string(), "1.5");
    }

    #[test]
    fn scalar_display_str() {
        // No surrounding quotes — the author writes them in the template if needed.
        assert_eq!(Scalar::Str("x".to_string()).to_string(), "x");
    }

    #[test]
    fn scalar_display_bool_true() {
        assert_eq!(Scalar::Bool(true).to_string(), "True");
    }

    #[test]
    fn scalar_display_bool_false() {
        assert_eq!(Scalar::Bool(false).to_string(), "False");
    }

    // --- validate_and_bind ---

    fn int_param(name: &str, min: Option<f64>, max: Option<f64>) -> crate::profile::Param {
        crate::profile::Param {
            name: name.to_string(),
            ty: crate::profile::ParamType::Int,
            min,
            max,
            enum_values: None,
        }
    }

    fn float_param(name: &str, min: Option<f64>, max: Option<f64>) -> crate::profile::Param {
        crate::profile::Param {
            name: name.to_string(),
            ty: crate::profile::ParamType::Float,
            min,
            max,
            enum_values: None,
        }
    }

    fn string_param(name: &str, enum_values: Option<Vec<String>>) -> crate::profile::Param {
        crate::profile::Param {
            name: name.to_string(),
            ty: crate::profile::ParamType::String,
            min: None,
            max: None,
            enum_values,
        }
    }

    fn bool_param(name: &str) -> crate::profile::Param {
        crate::profile::Param {
            name: name.to_string(),
            ty: crate::profile::ParamType::Bool,
            min: None,
            max: None,
            enum_values: None,
        }
    }

    #[test]
    fn bind_valid_ints() {
        let params = vec![
            int_param("joint", Some(0.0), Some(5.0)),
            int_param("angle", Some(0.0), Some(180.0)),
        ];
        let args = serde_json::json!({ "joint": 2, "angle": 90 });
        let bound = validate_and_bind(&args, &params).unwrap();
        assert_eq!(bound["joint"], Scalar::Int(2));
        assert_eq!(bound["angle"], Scalar::Int(90));
    }

    #[test]
    fn bind_missing_param_is_err() {
        let params = vec![int_param("joint", Some(0.0), Some(5.0))];
        let args = serde_json::json!({});
        assert!(validate_and_bind(&args, &params).is_err());
    }

    #[test]
    fn bind_wrong_type_is_err() {
        let params = vec![int_param("joint", Some(0.0), Some(5.0))];
        let args = serde_json::json!({ "joint": "two" });
        assert!(validate_and_bind(&args, &params).is_err());
    }

    #[test]
    fn bind_out_of_range_int_is_err() {
        let params = vec![int_param("joint", Some(0.0), Some(5.0))];
        let args = serde_json::json!({ "joint": 9 });
        assert!(validate_and_bind(&args, &params).is_err());
    }

    #[test]
    fn bind_out_of_range_float_is_err() {
        let params = vec![float_param("ratio", Some(0.0), Some(1.0))];
        let args = serde_json::json!({ "ratio": 1.5 });
        assert!(validate_and_bind(&args, &params).is_err());
    }

    #[test]
    fn bind_string_not_in_enum_is_err() {
        let params = vec![string_param(
            "mode",
            Some(vec!["a".to_string(), "b".to_string()]),
        )];
        let args = serde_json::json!({ "mode": "c" });
        assert!(validate_and_bind(&args, &params).is_err());
    }

    #[test]
    fn bind_string_in_enum_ok() {
        let params = vec![string_param(
            "mode",
            Some(vec!["a".to_string(), "b".to_string()]),
        )];
        let args = serde_json::json!({ "mode": "a" });
        let bound = validate_and_bind(&args, &params).unwrap();
        assert_eq!(bound["mode"], Scalar::Str("a".to_string()));
    }

    #[test]
    fn bind_bool_ok() {
        let params = vec![bool_param("enabled")];
        let args = serde_json::json!({ "enabled": true });
        let bound = validate_and_bind(&args, &params).unwrap();
        assert_eq!(bound["enabled"], Scalar::Bool(true));
    }

    #[test]
    fn bind_extra_key_is_err() {
        let params = vec![int_param("joint", Some(0.0), Some(5.0))];
        let args = serde_json::json!({ "joint": 2, "unexpected": 99 });
        assert!(validate_and_bind(&args, &params).is_err());
    }

    // --- render ---

    #[test]
    fn render_substitutes_placeholders() {
        let mut bound = BTreeMap::new();
        bound.insert("joint".to_string(), Scalar::Int(2));
        bound.insert("angle".to_string(), Scalar::Int(90));
        let result = render("servo.servo[{joint}].angle = {angle}", &bound).unwrap();
        assert_eq!(result, "servo.servo[2].angle = 90");
    }

    #[test]
    fn render_unknown_key_is_err() {
        let mut bound = BTreeMap::new();
        bound.insert("joint".to_string(), Scalar::Int(2));
        // Template references {angle} but it's not in bound
        assert!(render("{angle}", &bound).is_err());
    }

    #[test]
    fn render_no_placeholders_passthrough() {
        let bound: BTreeMap<String, Scalar> = BTreeMap::new();
        assert_eq!(render("home()", &bound).unwrap(), "home()");
    }
}
