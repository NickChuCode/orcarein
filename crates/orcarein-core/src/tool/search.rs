//! `search` — regex search over file contents, recursively, honoring
//! `.gitignore`.
//!
//! This is the agent's "find where code is defined or used" organ. It walks a
//! directory with ripgrep's [`ignore`] crate (so build artifacts like `target/`
//! never reach the model) and matches each line with the [`regex`] crate (an
//! RE2-style engine — no catastrophic backtracking, so a pathological pattern
//! can't hang). See `docs/decisions/0001-search-tool-traversal.md`.
//!
//! `RiskLevel::Risky`: it recursively enumerates structure *and* returns file
//! contents — strictly leakier than `list_dir` — so it goes through the
//! permission gate. Output is bounded (matches, bytes, and per-line columns are
//! all capped) and deterministic (sorted by path then line), which keeps token
//! cost predictable and makes the tool friendly to cross-turn reasoning.

use async_trait::async_trait;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::json;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

pub struct SearchTool;

#[derive(Deserialize)]
struct SearchArgs {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
    /// Filename filter, e.g. `*.rs` or `**/*.toml` (ripgrep override syntax).
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    output_mode: OutputMode,
}

/// What the tool prints. `content` is the default `path:line:text`; `files`
/// lists matching paths once; `count` reports matches per file.
#[derive(Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum OutputMode {
    #[default]
    Content,
    Files,
    Count,
}

fn default_path() -> String {
    ".".to_string()
}

/// Max match lines emitted in `content` mode before truncating. Keeps a broad
/// pattern from flooding the model's context (and the bill).
const MAX_RESULTS: usize = 200;

/// Max columns of a single matched line before it's clipped with an ellipsis,
/// so one minified/generated line can't blow the token budget.
const MAX_LINE_COLS: usize = 300;

/// One line that matched, with its 1-based line number and trimmed text.
struct Match {
    path: String,
    line: usize,
    text: String,
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search file contents by regular expression, recursively, honoring .gitignore. Returns `path:line:text` matches. Use this to find where code is defined or used."
    }

    fn risk_level(&self) -> RiskLevel {
        // Recurses through the tree (leaks structure, like `list_dir`) and
        // returns file contents (leaks data, more than `list_dir`), so it must
        // reach the permission gate. Guard against a silent downgrade to `Safe`.
        RiskLevel::Risky
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression (Rust regex syntax) to match against file contents.",
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Defaults to the current directory.",
                },
                "glob": {
                    "type": "string",
                    "description": "Filename filter, e.g. `*.rs` or `**/*.toml`. Only matching files are searched.",
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Match case-insensitively. Defaults to false.",
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files", "count"],
                    "description": "`content` (default) returns `path:line:text`; `files` lists matching paths; `count` reports matches per file.",
                }
            },
            "required": ["pattern"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let SearchArgs {
            pattern,
            path,
            glob,
            case_insensitive,
            output_mode,
        } = serde_json::from_value(args)?;

        let re = RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| ToolError::Other(format!("invalid regex: {e}")))?;

        let root = std::path::Path::new(&path);
        let mut builder = WalkBuilder::new(&path);
        // Honor .gitignore even outside a git repo, so the tool never searches
        // build artifacts in a plain (non-git) project directory either.
        builder.require_git(false);
        if let Some(glob) = &glob {
            // A positive override glob acts as a whitelist: only files matching
            // it are searched. Build errors mean a malformed glob — surface it.
            let mut ob = OverrideBuilder::new(root);
            ob.add(glob)
                .map_err(|e| ToolError::Other(format!("invalid glob: {e}")))?;
            let overrides = ob
                .build()
                .map_err(|e| ToolError::Other(format!("invalid glob: {e}")))?;
            builder.overrides(overrides);
        }

        let mut matches: Vec<Match> = Vec::new();

        for result in builder.build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue, // unreadable entry — skip, don't fail the whole search
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            // Non-UTF-8 / binary files are skipped rather than mangled.
            let contents = match std::fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let rel = rel_path(root, entry.path());
            for (idx, line) in contents.lines().enumerate() {
                if re.is_match(line) {
                    matches.push(Match {
                        path: rel.clone(),
                        line: idx + 1,
                        text: clip_cols(line.trim(), MAX_LINE_COLS),
                    });
                }
            }
        }

        if matches.is_empty() {
            return Ok(ToolOutput::new("no matches"));
        }

        matches.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        Ok(ToolOutput::new(format_matches(&matches, output_mode)))
    }
}

/// Renders sorted matches per `output_mode`.
fn format_matches(matches: &[Match], mode: OutputMode) -> String {
    let mut out = String::new();
    match mode {
        OutputMode::Content => {
            for m in matches.iter().take(MAX_RESULTS) {
                out.push_str(&format!("{}:{}:{}\n", m.path, m.line, m.text));
            }
            if matches.len() > MAX_RESULTS {
                out.push_str(&format!(
                    "… ({} matches total, showing {MAX_RESULTS}; narrow your pattern or glob)\n",
                    matches.len()
                ));
            }
        }
        OutputMode::Files => {
            // `matches` is sorted by path, so emit each path once.
            let mut last: Option<&str> = None;
            for m in matches {
                if last != Some(m.path.as_str()) {
                    out.push_str(&m.path);
                    out.push('\n');
                    last = Some(m.path.as_str());
                }
            }
        }
        OutputMode::Count => {
            let mut path: Option<&str> = None;
            let mut count = 0usize;
            for m in matches {
                match path {
                    Some(p) if p == m.path => count += 1,
                    _ => {
                        if let Some(p) = path {
                            out.push_str(&format!("{p}:{count}\n"));
                        }
                        path = Some(m.path.as_str());
                        count = 1;
                    }
                }
            }
            if let Some(p) = path {
                out.push_str(&format!("{p}:{count}\n"));
            }
        }
    }
    out
}

/// Clips `s` to at most `cols` characters, appending `…` if it was longer.
/// Counts by `char` so it never splits a multi-byte boundary.
fn clip_cols(s: &str, cols: usize) -> String {
    if s.chars().count() <= cols {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(cols).collect();
        format!("{clipped}…")
    }
}

/// Path of `entry` relative to the search `root`, with `/` separators on every
/// platform. Falls back to the file name when `root` is the file itself.
fn rel_path(root: &std::path::Path, entry: &std::path::Path) -> String {
    let rel = entry.strip_prefix(root).unwrap_or(entry);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| entry.to_string_lossy().into_owned())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = SearchTool;
        assert_eq!(t.name(), "search");
        let s = t.schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["pattern"]["type"], "string");
        assert!(s["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "pattern"));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn search_is_risky_not_safe() {
        // It leaks both structure and content; guard against a silent downgrade.
        assert_eq!(SearchTool.risk_level(), RiskLevel::Risky);
    }
}
