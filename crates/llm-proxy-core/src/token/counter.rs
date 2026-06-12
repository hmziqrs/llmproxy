//! Simplified token counter.
//!
//! Estimates token counts using a character-based heuristic (~4 chars/token)
//! instead of a full BPE encoding. The overhead constants mirror those used in
//! the Go reference implementation so that unit-test expectations stay aligned.

/// A single chat message with a role and content string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageContent {
    /// The role of the message author (e.g. `"user"`, `"assistant"`, `"system"`).
    ///
    /// Role validation is the caller's responsibility. The counter does not
    /// enforce valid role strings.
    pub role: String,
    /// The text content of the message.
    pub content: String,
}

impl MessageContent {
    /// Create a new message from a role and content string.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Approximate number of characters per token for estimation purposes.
///
/// This is a rough heuristic; actual token counts vary by tokenizer and
/// language. Multi-byte UTF-8 characters are counted individually (not by
/// byte length), so CJK text and emoji get more accurate estimates.
const CHARS_PER_TOKEN: usize = 4;

/// Simplified token counter.
///
/// Uses a rough heuristic of [`CHARS_PER_TOKEN`] characters per token rather
/// than a full BPE encoding. Good enough for cost estimation and rate-limiting
/// decisions where exact counts are not required.
#[derive(Debug, Clone, Default)]
pub struct Counter;

impl Counter {
    /// Create a new counter.
    pub fn new() -> Self {
        Self
    }

    /// Estimate the number of tokens in a single text string.
    ///
    /// Uses the approximation of [`CHARS_PER_TOKEN`] characters per token,
    /// counting Unicode code points (not bytes). Returns a minimum of 1 token
    /// for any non-empty string, including whitespace-only input.
    pub fn count_tokens(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        // Count Unicode code points, not bytes, for better accuracy with
        // multi-byte characters (CJK, emoji, etc.).
        let estimate = text.chars().count() / CHARS_PER_TOKEN;
        // Always count at least 1 token for non-empty text.
        estimate.max(1)
    }

    /// Estimate the total token count for a chat completion request.
    ///
    /// The formula mirrors the Go reference implementation:
    ///
    /// ```text
    /// total = BASE_TOKENS + system_tokens + SYSTEM_OVERHEAD
    ///       + sum(count_tokens(role) + count_tokens(content) + PER_MESSAGE_OVERHEAD)
    /// ```
    ///
    /// The extra [`PER_MESSAGE_OVERHEAD`] per message accounts for formatting
    /// overhead (role labels, separator tokens, etc.).
    ///
    /// Note: `system` is a flat string. If the upstream protocol supports
    /// multiple system blocks, the caller must pre-concatenate them.
    /// Role validation is the caller's responsibility.
    pub fn count_messages(&self, system: &str, messages: &[MessageContent]) -> usize {
        // Mirrors the Go reference implementation constants:
        //   BASE_TOKENS = 3, PER_MESSAGE_OVERHEAD = 5, SYSTEM_OVERHEAD = 5
        const BASE_TOKENS: usize = 3;
        const PER_MESSAGE_OVERHEAD: usize = 5;
        const SYSTEM_OVERHEAD: usize = 5;

        let system_tokens = self.count_tokens(system);

        let message_tokens: usize = messages
            .iter()
            .map(|msg| {
                self.count_tokens(&msg.role)
                    + self.count_tokens(&msg.content)
                    + PER_MESSAGE_OVERHEAD
            })
            .sum();

        BASE_TOKENS + system_tokens + SYSTEM_OVERHEAD + message_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_tokens_empty_string() {
        let counter = Counter::new();
        assert_eq!(counter.count_tokens(""), 0);
    }

    #[test]
    fn count_tokens_single_char() {
        let counter = Counter::new();
        // 1 char / 4 = 0, but minimum is 1 for non-empty.
        assert_eq!(counter.count_tokens("a"), 1);
    }

    #[test]
    fn count_tokens_short_text() {
        let counter = Counter::new();
        // "hello" = 5 chars, 5/4 = 1.
        assert_eq!(counter.count_tokens("hello"), 1);
    }

    #[test]
    fn count_tokens_medium_text() {
        let counter = Counter::new();
        // 20 chars / 4 = 5 tokens.
        assert_eq!(counter.count_tokens("a".repeat(20).as_str()), 5);
    }

    #[test]
    fn count_tokens_longer_text() {
        let counter = Counter::new();
        // 100 chars / 4 = 25 tokens.
        assert_eq!(counter.count_tokens("a".repeat(100).as_str()), 25);
    }

    #[test]
    fn count_messages_no_messages() {
        let counter = Counter::new();
        // base(3) + system_tokens(4/4=1) + system_overhead(5) + 0 messages
        // = 3 + 1 + 5 = 9
        let total = counter.count_messages("test", &[]);
        assert_eq!(total, 9);
    }

    #[test]
    fn count_messages_empty_system() {
        let counter = Counter::new();
        // base(3) + system_tokens(0) + system_overhead(5) + 0 messages = 8
        let total = counter.count_messages("", &[]);
        assert_eq!(total, 8);
    }

    #[test]
    fn count_messages_single_user_message() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "hello")];
        // base(3) + system(0) + sys_overhead(5)
        // + role("user"=4/4=1) + content("hello"=5/4=1) + per_msg_overhead(5)
        // = 3 + 0 + 5 + 1 + 1 + 5 = 15
        let total = counter.count_messages("", &messages);
        assert_eq!(total, 15);
    }

    #[test]
    fn count_messages_multiple_messages() {
        let counter = Counter::new();
        let messages = vec![
            MessageContent::new("system", "You are a helpful assistant."),
            MessageContent::new("user", "Hello, how are you?"),
            MessageContent::new("assistant", "I'm doing well, thank you!"),
        ];
        let total = counter.count_messages("Be helpful", &messages);
        // Manual calculation:
        // base = 3
        // system = "Be helpful" = 10 chars / 4 = 2 tokens
        // system_overhead = 5
        //
        // msg0: role="system"(6/4=1) + content="You are a helpful assistant."(27/4=6) + 5 = 12
        // msg1: role="user"(4/4=1) + content="Hello, how are you?"(20/4=5) + 5 = 11
        // msg2: role="assistant"(9/4=2) + content="I'm doing well, thank you!"(27/4=6) + 5 = 13
        //
        // total = 3 + 2 + 5 + 12 + 11 + 13 = 46
        assert_eq!(total, 46);
    }

    #[test]
    fn count_messages_with_system_prompt() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "Hi")];
        // base(3) + system("You are helpful"=15/4=3) + sys_overhead(5)
        // + role("user"=4/4=1) + content("Hi"=2/4=0->1) + per_msg_overhead(5)
        // = 3 + 3 + 5 + 1 + 1 + 5 = 18
        let total = counter.count_messages("You are helpful", &messages);
        assert_eq!(total, 18);
    }

    #[test]
    fn message_content_new() {
        let msg = MessageContent::new("user", "test content");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "test content");
    }

    #[test]
    fn counter_default_is_valid() {
        let counter = Counter;
        assert_eq!(counter.count_tokens("hello"), 1);
    }

    #[test]
    fn count_messages_empty_role() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("", "hello")];
        // base(3) + system(0) + sys_overhead(5) + role("") + content(1) + per_msg(5)
        // = 3 + 0 + 5 + 0 + 1 + 5 = 14
        let total = counter.count_messages("", &messages);
        assert_eq!(total, 14);
    }

    #[test]
    fn count_messages_empty_content() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "")];
        // base(3) + system(0) + sys_overhead(5) + role(1) + content(0) + per_msg(5)
        // = 3 + 0 + 5 + 1 + 0 + 5 = 14
        let total = counter.count_messages("", &messages);
        assert_eq!(total, 14);
    }

    #[test]
    fn count_tokens_whitespace_only() {
        let counter = Counter::new();
        // Whitespace-only string is non-empty, so minimum 1 token.
        assert_eq!(counter.count_tokens("   "), 1);
        assert_eq!(counter.count_tokens("\t\n"), 1);
    }

    #[test]
    fn count_tokens_multibyte_unicode() {
        let counter = Counter::new();
        // Emoji: U+1F600 is 4 bytes but 1 code point. 1/4 = 0 -> max(0,1) = 1.
        assert_eq!(counter.count_tokens("\u{1F600}"), 1);
        // CJK string: 4 characters (each 3 bytes in UTF-8). 4/4 = 1 token.
        assert_eq!(counter.count_tokens("\u{4F60}\u{597D}\u{4E16}\u{754C}"), 1);
        // 8 CJK characters: 8/4 = 2 tokens.
        let cjk_8: String = "\u{4F60}\u{597D}\u{4E16}\u{754C}\u{4F60}\u{597D}\u{4E16}\u{754C}"
            .to_string();
        assert_eq!(counter.count_tokens(&cjk_8), 2);
    }

    #[test]
    fn count_tokens_mixed_ascii_unicode() {
        let counter = Counter::new();
        // "hello" (5 chars) + emoji (1 char) = 6 chars. 6/4 = 1.
        assert_eq!(counter.count_tokens("hello\u{1F600}"), 1);
    }
}
