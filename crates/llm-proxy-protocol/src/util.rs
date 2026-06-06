//! Shared utility functions used across multiple crates.
//!
//! This module contains small helper functions that are needed by more than one
//! crate (e.g., `llm-proxy-provider` and `llm-proxy-server`) to avoid code
//! duplication.

/// Truncate a string to `max_len` bytes, appending `suffix` if truncation occurs.
///
/// The final string is at most `max_len` bytes (the suffix is included within
/// this budget). Respects UTF-8 char boundaries to prevent panics on multi-byte
/// characters.
///
/// # Panics
///
/// Panics if `suffix.len() > max_len` (the suffix alone would exceed the budget).
pub fn truncate_with_suffix(s: &str, max_len: usize, suffix: &str) -> String {
    if s.len() <= max_len {
        s.to_owned()
    } else {
        let max_content = max_len - suffix.len();
        let mut end = max_content;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}{}", &s[..end], suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_when_short_enough() {
        assert_eq!(truncate_with_suffix("hello", 10, "..."), "hello");
    }

    #[test]
    fn truncates_with_suffix() {
        assert_eq!(truncate_with_suffix("hello world!", 8, "..."), "hello...");
    }

    #[test]
    fn respects_utf8_boundaries() {
        // "é" is 2 bytes in UTF-8
        let input = "éééééé"; // 12 bytes
        let result = truncate_with_suffix(input, 7, "...");
        assert!(result.len() <= 7);
        assert!(result.ends_with("..."));
    }
}
