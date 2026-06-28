//! OSC52 clipboard escape sequences with a hand-rolled base64 encoder.
//!
//! Terminals that support OSC52 let an application set the system clipboard by
//! emitting `ESC ] 52 ; c ; <base64> BEL`. Many terminals silently drop
//! oversized sequences, so [`osc52_sequence`] caps the payload at 64 KiB
//! (spec §8); the internal register still works regardless. No external deps:
//! we hand-roll base64 to keep the dependency surface flat (MSRV 1.85, stdlib).

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Maximum payload size (pre-encoding) for an OSC52 write (spec §8).
const OSC52_MAX_BYTES: usize = 64 * 1024;

/// Encode bytes as standard base64 (RFC 4648 alphabet, `=` padding).
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        // Pack up to 3 bytes into a 24-bit big-endian buffer.
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(BASE64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Build the OSC52 set-clipboard escape sequence for `text`.
///
/// Returns `None` when `text` exceeds 64 KiB, since many terminals drop
/// oversized sequences (spec §8).
pub fn osc52_sequence(text: &str) -> Option<String> {
    if text.len() > OSC52_MAX_BYTES {
        return None;
    }
    Some(format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes())))
}

/// Write the OSC52 sequence for `text` to stdout and flush.
///
/// No-ops when `ORCAREIN_NO_OSC52` is set or when the payload exceeds the
/// 64 KiB cap. Thin I/O shell over [`osc52_sequence`]; not unit-tested.
pub fn write_osc52(text: &str) {
    if std::env::var_os("ORCAREIN_NO_OSC52").is_none() {
        if let Some(seq) = osc52_sequence(text) {
            use std::io::Write;
            print!("{seq}");
            let _ = std::io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_base64_with_esc_and_bel() {
        let s = osc52_sequence("foo").unwrap();
        assert_eq!(s, "\x1b]52;c;Zm9v\x07");
    }

    #[test]
    fn osc52_skipped_when_over_64kib() {
        let big = "a".repeat(64 * 1024 + 1);
        assert!(osc52_sequence(&big).is_none());
    }
}
