//! Best-effort repair of model-emitted tool-call arguments.
//!
//! DeepSeek's function-calling occasionally hands us an `arguments` string that
//! is not clean JSON: an empty string, JSON wrapped in a Markdown code fence,
//! or a JSON object with leading/trailing prose. Rather than fail the tool call
//! outright, [`parse_tool_arguments`] salvages the common cases. Whatever it
//! can't repair becomes a *self-correcting* error the agent loop feeds back to
//! the model (see `agent::Agent::run_tool`), so the model can retry.
//!
//! This is the "tool-call repair" layer — the robustness a self-bootstrapping
//! bot needs so a single malformed call doesn't strand it mid-task.

/// Parses a model-emitted tool `arguments` string into JSON, repairing the
/// malformations DeepSeek commonly emits.
///
/// Strategy, in order:
/// 1. An empty/whitespace string means "no arguments" → `{}`.
/// 2. Parse the trimmed string as-is.
/// 3. Extract the first balanced `{...}` object (handles Markdown fences and
///    surrounding prose) and parse that.
///
/// On failure returns a short, human/model-readable reason (not a raw serde
/// error dump).
pub fn parse_tool_arguments(raw: &str) -> Result<serde_json::Value, String> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(v);
    }

    if let Some(obj) = extract_first_json_object(trimmed) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
            return Ok(v);
        }
    }

    Err(format!("not valid JSON ({})", snippet(trimmed)))
}

/// Returns the first balanced `{...}` substring, accounting for nested braces
/// and braces inside string literals. `None` if there is no complete object.
fn extract_first_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;

    for (offset, c) in s[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + offset + c.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A short, single-line preview of an unparseable string for error messages.
fn snippet(s: &str) -> String {
    const MAX: usize = 60;
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > MAX {
        let head: String = line.chars().take(MAX).collect();
        format!("{head}…")
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_string_becomes_empty_object() {
        assert_eq!(parse_tool_arguments("").unwrap(), json!({}));
        assert_eq!(parse_tool_arguments("   \n").unwrap(), json!({}));
    }

    #[test]
    fn clean_json_passes_through() {
        assert_eq!(
            parse_tool_arguments(r#"{"path":"Cargo.toml"}"#).unwrap(),
            json!({ "path": "Cargo.toml" })
        );
    }

    #[test]
    fn strips_markdown_code_fence() {
        let raw = "```json\n{\"path\": \"src/main.rs\"}\n```";
        assert_eq!(
            parse_tool_arguments(raw).unwrap(),
            json!({ "path": "src/main.rs" })
        );
    }

    #[test]
    fn extracts_json_from_surrounding_prose() {
        let raw = "Sure! Here are the arguments: {\"cmd\": \"ls -la\"} — let me know.";
        assert_eq!(
            parse_tool_arguments(raw).unwrap(),
            json!({ "cmd": "ls -la" })
        );
    }

    #[test]
    fn handles_nested_objects_and_braced_strings() {
        let raw = r#"prefix {"a": {"b": 1}, "s": "has } brace"} suffix"#;
        assert_eq!(
            parse_tool_arguments(raw).unwrap(),
            json!({ "a": { "b": 1 }, "s": "has } brace" })
        );
    }

    #[test]
    fn unrepairable_returns_short_reason() {
        let err = parse_tool_arguments("this is not json at all").unwrap_err();
        assert!(err.starts_with("not valid JSON"), "{err}");
        // No raw serde noise; just a short preview.
        assert!(err.contains("this is not json"), "{err}");
    }

    #[test]
    fn incomplete_object_is_an_error() {
        // Truncated JSON (no closing brace) can't be salvaged.
        assert!(parse_tool_arguments(r#"{"path": "x""#).is_err());
    }
}
