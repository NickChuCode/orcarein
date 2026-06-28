//! Hand-rolled, zero-dependency syntax highlighter for code blocks (v02-30).
//! Produces semantic [`SynKind`] runs only — no color, no ratatui — so the
//! lexer is pure and unit-tested; `markdown.rs` maps kinds to Claude Design
//! colors. Single-line scanning (cross-line block comments not tracked).

/// A semantic syntax token kind. `Plain` = uncolored (default body fg).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynKind {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
}

/// One language's lexing profile.
struct Profile {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    keywords: &'static [&'static str],
}

/// Lexing profile for `lang` (lowercased fence info-string); generic fallback
/// (strings + numbers only) for unknown languages.
fn profile_for(lang: &str) -> Profile {
    const C_LINE: &[&str] = &["//"];
    const C_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));
    const HASH: &[&str] = &["#"];
    match lang {
        "rust" => Profile {
            line_comments: C_LINE,
            block_comment: C_BLOCK,
            keywords: &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
        },
        "js" | "ts" | "javascript" | "typescript" => Profile {
            line_comments: C_LINE,
            block_comment: C_BLOCK,
            keywords: &[
                "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
                "delete", "do", "else", "export", "extends", "false", "finally", "for", "function",
                "if", "import", "in", "instanceof", "let", "new", "null", "return", "super",
                "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "yield",
            ],
        },
        "go" => Profile {
            line_comments: C_LINE,
            block_comment: C_BLOCK,
            keywords: &[
                "break", "case", "chan", "const", "continue", "default", "defer", "else",
                "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
                "package", "range", "return", "select", "struct", "switch", "type", "var", "nil",
                "true", "false",
            ],
        },
        "c" | "cpp" | "c++" => Profile {
            line_comments: C_LINE,
            block_comment: C_BLOCK,
            keywords: &[
                "auto", "break", "case", "char", "const", "continue", "default", "do", "double",
                "else", "enum", "extern", "float", "for", "goto", "if", "int", "long", "return",
                "short", "signed", "sizeof", "static", "struct", "switch", "typedef", "union",
                "unsigned", "void", "volatile", "while", "class", "namespace", "template", "public",
                "private", "protected", "new", "delete", "true", "false", "nullptr",
            ],
        },
        "python" | "py" => Profile {
            line_comments: HASH,
            block_comment: None,
            keywords: &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
                "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
                "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
                "with", "yield", "True", "False", "None",
            ],
        },
        "bash" | "sh" | "shell" => Profile {
            line_comments: HASH,
            block_comment: None,
            keywords: &[
                "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
                "function", "in", "return", "local", "export", "echo",
            ],
        },
        "toml" => Profile {
            line_comments: HASH,
            block_comment: None,
            keywords: &["true", "false"],
        },
        "json" => Profile {
            line_comments: &[],
            block_comment: None,
            keywords: &["true", "false", "null"],
        },
        _ => Profile {
            line_comments: &[],
            block_comment: None,
            keywords: &[],
        },
    }
}

/// Split one code line into semantic runs. Pure; the run texts concatenate back
/// to `line` exactly. Priority (at run start, outside any open string):
/// comment > string > number > identifier(keyword/plain) > other plain.
pub fn highlight(line: &str, lang: &str) -> Vec<(String, SynKind)> {
    let p = profile_for(lang);
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut runs: Vec<(String, SynKind)> = Vec::new();
    let push = |runs: &mut Vec<(String, SynKind)>, lo: usize, hi: usize, kind: SynKind| {
        if lo >= hi {
            return;
        }
        let s: String = chars[lo..hi].iter().collect();
        // Coalesce with the previous run if same kind.
        if let Some((prev, pk)) = runs.last_mut() {
            if *pk == kind {
                prev.push_str(&s);
                return;
            }
        }
        runs.push((s, kind));
    };
    let starts_with = |i: usize, pat: &str| -> bool {
        let pc: Vec<char> = pat.chars().collect();
        i + pc.len() <= n && chars[i..i + pc.len()] == pc[..]
    };
    let mut i = 0;
    while i < n {
        // 1. line comment (outside strings — we only reach here at run starts)
        if p.line_comments.iter().any(|m| starts_with(i, m)) {
            push(&mut runs, i, n, SynKind::Comment);
            break;
        }
        // 2. single-line block comment
        if let Some((open, close)) = p.block_comment {
            if starts_with(i, open) {
                let from = i + open.chars().count();
                let mut j = from;
                let mut end = None;
                while j < n {
                    if starts_with(j, close) {
                        end = Some(j + close.chars().count());
                        break;
                    }
                    j += 1;
                }
                let stop = end.unwrap_or(n);
                push(&mut runs, i, stop, SynKind::Comment);
                i = stop;
                continue;
            }
        }
        let c = chars[i];
        // 3. string (escape-aware)
        if c == '"' || c == '\'' || c == '`' {
            let mut j = i + 1;
            let mut closed = false;
            while j < n {
                if chars[j] == '\\' {
                    j += 2; // skip escaped char (trailing \ runs past end → unclosed)
                    continue;
                }
                if chars[j] == c {
                    j += 1;
                    closed = true;
                    break;
                }
                j += 1;
            }
            let stop = if closed { j.min(n) } else { n };
            push(&mut runs, i, stop, SynKind::Str);
            i = stop;
            continue;
        }
        // 4. number (only when the cursor char is a digit)
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            push(&mut runs, i, j, SynKind::Number);
            i = j;
            continue;
        }
        // 5. identifier (consumes trailing digits)
        if c.is_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = if p.keywords.contains(&word.as_str()) {
                SynKind::Keyword
            } else {
                SynKind::Plain
            };
            push(&mut runs, i, j, kind);
            i = j;
            continue;
        }
        // 6. other char → plain
        push(&mut runs, i, i + 1, SynKind::Plain);
        i += 1;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, lang: &str) -> Vec<(String, SynKind)> {
        let out = highlight(line, lang);
        // Invariant: runs concatenate back to the input.
        let joined: String = out.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, line, "runs must reconstruct the line");
        out
    }

    fn find(runs: &[(String, SynKind)], text: &str) -> SynKind {
        runs.iter()
            .find(|(s, _)| s == text)
            .map(|(_, k)| *k)
            .unwrap()
    }

    #[test]
    fn rust_keyword_number_comment_string() {
        let r = kinds("let x = 1; // c", "rust");
        assert_eq!(find(&r, "let"), SynKind::Keyword);
        assert_eq!(find(&r, "1"), SynKind::Number);
        assert_eq!(find(&r, "// c"), SynKind::Comment);
        // escaped quote does not end the string early
        let r2 = kinds("\"a\\\"b\"", "rust");
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].1, SynKind::Str);
    }

    #[test]
    fn comment_markers_inside_strings_are_not_comments() {
        let r = kinds("let s = \"a // b\";", "rust");
        assert!(r.iter().any(|(s, k)| s == "\"a // b\"" && *k == SynKind::Str));
        assert!(!r.iter().any(|(_, k)| *k == SynKind::Comment));
        let p = kinds("x = \"# y\"", "python");
        assert!(p.iter().any(|(s, k)| s == "\"# y\"" && *k == SynKind::Str));
        assert!(!p.iter().any(|(_, k)| *k == SynKind::Comment));
    }

    #[test]
    fn identifier_with_trailing_digits_is_not_a_number() {
        let r = kinds("x1 = 2", "rust");
        // The trailing digit of `x1` stays part of the identifier: the only
        // Number run is the standalone `2` (Plain runs coalesce, so x1 merges
        // with the following ` = `).
        let nums: Vec<&str> = r
            .iter()
            .filter(|(_, k)| *k == SynKind::Number)
            .map(|(s, _)| s.as_str())
            .collect();
        assert_eq!(nums, vec!["2"]);
        assert!(r.iter().any(|(s, k)| s.starts_with("x1") && *k == SynKind::Plain));
    }

    #[test]
    fn python_hash_comment_and_keyword() {
        let r = kinds("def f(): # c", "python");
        assert_eq!(find(&r, "def"), SynKind::Keyword);
        assert_eq!(find(&r, "# c"), SynKind::Comment);
    }

    #[test]
    fn unknown_lang_marks_only_strings_and_numbers() {
        let r = kinds("foo \"bar\" 42 // x", "weirdlang");
        assert!(r.iter().any(|(s, k)| s == "\"bar\"" && *k == SynKind::Str));
        assert!(r.iter().any(|(s, k)| s == "42" && *k == SynKind::Number));
        assert!(!r.iter().any(|(_, k)| *k == SynKind::Comment));
    }

    #[test]
    fn numbers_consume_hex_dot_underscore() {
        assert_eq!(kinds("0xFF", "rust")[0].1, SynKind::Number);
        assert_eq!(kinds("3.14", "rust")[0].1, SynKind::Number);
        assert_eq!(kinds("1_000", "rust")[0].1, SynKind::Number);
    }
}
