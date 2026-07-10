//! Small text helpers shared across tools.
//!
//! `cap` bounds an oversized tool/output string so a single noisy result
//! (a huge `bash` dump, a big MCP a11y snapshot) can't flood the model's
//! context window. Extracted from `tool::bash` so the MCP tool can reuse the
//! exact same, char-boundary-safe, byte-counted truncation.

/// Truncate `s` to at most `max_bytes` on a char boundary, appending a notice
/// when anything was dropped. Returns the input unchanged when it already fits.
/// Char-boundary safe (never splits a multibyte UTF-8 sequence).
///
/// Note: the appended notice means the returned string can be slightly longer
/// than `max_bytes`; the cap bounds the *retained content*, not the notice.
pub fn cap(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    format!(
        "{}\n…[truncated {dropped} bytes; {} total]",
        &s[..end],
        s.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_leaves_short_output_unchanged() {
        assert_eq!(cap("hello", 1024), "hello");
        assert_eq!(cap("", 1024), "");
    }

    #[test]
    fn cap_truncates_long_output_with_notice() {
        let big = "a".repeat(100);
        let out = cap(&big, 10);
        assert!(out.starts_with(&"a".repeat(10)));
        assert!(out.contains("truncated 90 bytes; 100 total"));
        assert!(out.len() < big.len() + 64);
    }

    #[test]
    fn cap_never_splits_a_multibyte_char() {
        // "中文" is 6 bytes (3 each); capping at 2 lands mid-"中", so it must
        // back up to the boundary at 0.
        let out = cap("中文", 2);
        assert_eq!(out, "\n…[truncated 6 bytes; 6 total]");
        let out3 = cap("中文", 3);
        assert_eq!(out3, "中\n…[truncated 3 bytes; 6 total]");
    }
}
