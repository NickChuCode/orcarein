//! Placeholder scanning shared by profile validation (Task 2) and the runtime
//! renderer (added in a later task).
use crate::error::HardwareError;

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
}
