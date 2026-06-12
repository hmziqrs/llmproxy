//! Shared utility functions for the protocol crate.
//!
//! Contains small helper functions used internally by the protocol adapters.

/// Truncate a string to `max_len` bytes, appending `suffix` if truncation occurs.
///
/// The final string is at most `max_len` bytes (the suffix is included within
/// this budget). Respects UTF-8 char boundaries to prevent panics on multi-byte
/// characters.
///
/// When a multi-byte character straddles the truncation boundary, the function
/// backs up to the previous character boundary, so the output may be shorter
/// than `max_len` by up to 3 bytes (the maximum UTF-8 character width).
///
/// If `suffix.len() >= max_len`, the suffix alone would exceed the budget, so
/// the original string is returned untruncated (the caller's intent cannot be
/// satisfied safely).
pub fn truncate_with_suffix(s: &str, max_len: usize, suffix: &str) -> String {
    if s.len() <= max_len {
        return s.to_owned();
    }
    // If the suffix alone would exceed or exactly fill the budget, we cannot
    // produce a meaningful truncated result -- return the original string.
    if suffix.len() >= max_len {
        return s.to_owned();
    }
    let max_content = max_len - suffix.len();
    let mut end = max_content;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}{}", &s[..end], suffix)
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

    #[test]
    fn suffix_equals_max_len_returns_original() {
        // suffix.len() == max_len: cannot fit any content, return original
        assert_eq!(truncate_with_suffix("hello", 3, "..."), "hello");
    }

    #[test]
    fn suffix_exceeds_max_len_returns_original() {
        // suffix.len() > max_len: suffix alone exceeds budget, return original
        assert_eq!(truncate_with_suffix("hello", 2, "..."), "hello");
    }

    #[test]
    fn empty_input_with_suffix() {
        assert_eq!(truncate_with_suffix("", 5, "..."), "");
    }

    #[test]
    fn empty_suffix_truncates_cleanly() {
        assert_eq!(truncate_with_suffix("hello world", 5, ""), "hello");
    }

    #[test]
    fn exactly_at_max_len_no_truncation() {
        assert_eq!(truncate_with_suffix("hello", 5, "..."), "hello");
    }

    #[test]
    fn max_len_equals_suffix_len_plus_one() {
        // "abcdef" is 6 bytes, max_len=4, suffix="..." (3 bytes) -> max_content=1
        // Only 1 byte of content fits before the suffix.
        assert_eq!(truncate_with_suffix("abcdef", 4, "..."), "a...");
    }

    #[test]
    fn utf8_boundary_backup_produces_valid_string() {
        // Input: "αβγδε" (each char 2 bytes = 10 bytes total)
        // max_len=7, suffix="..." (3 bytes) -> max_content=4
        // byte 4 is mid-character in "αβ" (α=0-1, β=2-3, γ=4-5) so end backs to 4
        let input = "αβγδε";
        let result = truncate_with_suffix(input, 7, "...");
        assert!(result.len() <= 7);
        assert!(result.ends_with("..."));
        // Content before suffix should be valid UTF-8
        let content = &result[..result.len() - 3];
        assert!(content.is_char_boundary(content.len()));
    }
}
