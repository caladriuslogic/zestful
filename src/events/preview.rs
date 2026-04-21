//! Preview truncation and sha256 hashing helpers used when emitting
//! `*_preview` + `*_hash` pairs on events.

use sha2::{Digest, Sha256};

/// Truncate `s` to at most `max_bytes` bytes, respecting UTF-8 codepoint
/// boundaries. If truncation happens, returns the longest valid-UTF-8 prefix
/// that fits. Never panics; never returns an invalid UTF-8 string.
pub fn truncate_utf8_safe(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk char boundaries back from max_bytes until we find one.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// sha256 of the input string, lowercase hex.
pub fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_utf8_safe("hello", 100), "hello");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate_utf8_safe("hello", 5), "hello");
    }

    #[test]
    fn truncate_ascii_simple() {
        assert_eq!(truncate_utf8_safe("hello world", 5), "hello");
    }

    #[test]
    fn truncate_respects_codepoint_boundary() {
        // "héllo" = 6 bytes (h, é=2 bytes, l, l, o). Truncating to 2 bytes
        // would split é; must drop back to 1 byte ("h").
        let s = "héllo";
        assert_eq!(truncate_utf8_safe(s, 2), "h");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate_utf8_safe("", 10), "");
    }

    #[test]
    fn truncate_zero_budget() {
        assert_eq!(truncate_utf8_safe("anything", 0), "");
    }

    #[test]
    fn sha256_hex_known_vector() {
        // sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_hex_empty_string() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_is_64_chars() {
        assert_eq!(sha256_hex("anything").len(), 64);
    }
}
