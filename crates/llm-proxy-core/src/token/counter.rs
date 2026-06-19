//! Token counter with model-aware BPE dispatch.
//!
//! [`Counter`] is the public front door for token counting. For known OpenAI
//! model ids it dispatches to a real BPE tokenizer (via
//! [`super::tiktoken`]); for everything else it falls back to a character
//! heuristic (~4 chars/token). The BPE ranks are bundled at compile time, so
//! counting is offline-safe and the heuristic is always available as a
//! last resort.
//!
//! The overhead constants (base tokens, per-message overhead, system overhead)
//! mirror the Go reference implementation so unit-test expectations stay
//! aligned.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::token::tiktoken::{self, Encoding, HeuristicTokenizer, TiktokenTokenizer, Tokenizer};

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

/// Model-aware token counter.
///
/// Holds the heuristic backend and a lazily-populated cache of loaded BPE
/// tokenizers keyed by [`Encoding`]. The cache uses interior mutability so a
/// `&Counter` (shared via `Arc` in `AppState`) can load encodings on first use.
/// Cloning a `Counter` shares the cache (the `Mutex` lives behind an `Arc`).
#[derive(Clone, Default)]
pub struct Counter {
    /// Heuristic fallback for unknown models and load failures.
    heuristic: HeuristicTokenizer,
    /// Lazily-loaded BPE tokenizers, keyed by encoding. Cheap to share: clone
    /// is an `Arc` refcount bump.
    cache: Arc<Mutex<HashMap<Encoding, Arc<TiktokenTokenizer>>>>,
}

impl std::fmt::Debug for Counter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Report how many encodings are loaded rather than dumping the (large)
        // BPE tables or fighting `Mutex`'s `Debug` bound.
        let loaded = self.cache.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("Counter")
            .field("backend", &"bpe+heuristic")
            .field("loaded_encodings", &loaded)
            .finish()
    }
}

impl Counter {
    /// Create a new counter with an empty BPE cache (encodings load lazily).
    pub fn new() -> Self {
        Self::default()
    }

    /// Count tokens in `text` using the model's BPE encoding when the model is
    /// known, otherwise the heuristic.
    ///
    /// BPE encodings load lazily on first use for a known model and are cached
    /// for subsequent calls. If an encoding cannot be loaded (offline, corrupt
    /// ranks), the heuristic is used so a request never fails on the tokenizer.
    pub fn count_tokens(&self, model: &str, text: &str) -> usize {
        match tiktoken::encoding_for_model(model) {
            Some(encoding) => match self.bpe_for(encoding) {
                Some(tokenizer) => tokenizer.count_tokens(text),
                // Known model but the ranks could not be loaded: heuristic.
                None => self.heuristic.count_tokens(text),
            },
            None => self.heuristic.count_tokens(text),
        }
    }

    /// Get the cached BPE tokenizer for an encoding, loading it lazily on first
    /// use. Returns `None` only if the ranks could not be loaded.
    fn bpe_for(&self, encoding: Encoding) -> Option<Arc<TiktokenTokenizer>> {
        // Fast path: already loaded.
        if let Ok(cache) = self.cache.lock() {
            if let Some(tokenizer) = cache.get(&encoding).cloned() {
                return Some(tokenizer);
            }
        }
        // Slow path: load outside the lock, then insert.
        let tokenizer = Arc::new(TiktokenTokenizer::new(encoding.load()?));
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(encoding, Arc::clone(&tokenizer));
        }
        Some(tokenizer)
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
    /// Each piece is counted with the model-appropriate backend (BPE for known
    /// OpenAI models, heuristic otherwise).
    ///
    /// Note: `system` is a flat string. If the upstream protocol supports
    /// multiple system blocks, the caller must pre-concatenate them.
    /// Role validation is the caller's responsibility.
    pub fn count_messages(&self, model: &str, system: &str, messages: &[MessageContent]) -> usize {
        // Mirrors the Go reference implementation constants:
        //   BASE_TOKENS = 3, PER_MESSAGE_OVERHEAD = 5, SYSTEM_OVERHEAD = 5
        const BASE_TOKENS: usize = 3;
        const PER_MESSAGE_OVERHEAD: usize = 5;
        const SYSTEM_OVERHEAD: usize = 5;

        let system_tokens = self.count_tokens(model, system);

        let message_tokens: usize = messages
            .iter()
            .map(|msg| {
                self.count_tokens(model, &msg.role)
                    + self.count_tokens(model, &msg.content)
                    + PER_MESSAGE_OVERHEAD
            })
            .sum();

        BASE_TOKENS + system_tokens + SYSTEM_OVERHEAD + message_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unknown model id that matches no [`Encoding`] prefix, so `Counter`
    /// routes it to the heuristic backend.
    const HEURISTIC_MODEL: &str = "unknown-test-model";

    #[test]
    fn count_tokens_empty_string() {
        let counter = Counter::new();
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, ""), 0);
    }

    #[test]
    fn count_tokens_single_char() {
        let counter = Counter::new();
        // 1 char / 4 = 0, but minimum is 1 for non-empty.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "a"), 1);
    }

    #[test]
    fn count_tokens_short_text() {
        let counter = Counter::new();
        // "hello" = 5 chars, 5/4 = 1.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "hello"), 1);
    }

    #[test]
    fn count_tokens_medium_text() {
        let counter = Counter::new();
        // 20 chars / 4 = 5 tokens.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, &"a".repeat(20)), 5);
    }

    #[test]
    fn count_tokens_longer_text() {
        let counter = Counter::new();
        // 100 chars / 4 = 25 tokens.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, &"a".repeat(100)), 25);
    }

    #[test]
    fn count_messages_no_messages() {
        let counter = Counter::new();
        // base(3) + system_tokens(4/4=1) + system_overhead(5) + 0 messages
        // = 3 + 1 + 5 = 9
        let total = counter.count_messages(HEURISTIC_MODEL, "test", &[]);
        assert_eq!(total, 9);
    }

    #[test]
    fn count_messages_empty_system() {
        let counter = Counter::new();
        // base(3) + system_tokens(0) + system_overhead(5) + 0 messages = 8
        let total = counter.count_messages(HEURISTIC_MODEL, "", &[]);
        assert_eq!(total, 8);
    }

    #[test]
    fn count_messages_single_user_message() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "hello")];
        // base(3) + system(0) + sys_overhead(5)
        // + role("user"=4/4=1) + content("hello"=5/4=1) + per_msg_overhead(5)
        // = 3 + 0 + 5 + 1 + 1 + 5 = 15
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
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
        let total = counter.count_messages(HEURISTIC_MODEL, "Be helpful", &messages);
        // Manual calculation (heuristic):
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
        let total = counter.count_messages(HEURISTIC_MODEL, "You are helpful", &messages);
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
        let counter = Counter::new();
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "hello"), 1);
    }

    #[test]
    fn count_messages_empty_role() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("", "hello")];
        // base(3) + system(0) + sys_overhead(5) + role("") + content(1) + per_msg(5)
        // = 3 + 0 + 5 + 0 + 1 + 5 = 14
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
        assert_eq!(total, 14);
    }

    #[test]
    fn count_messages_empty_content() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "")];
        // base(3) + system(0) + sys_overhead(5) + role(1) + content(0) + per_msg(5)
        // = 3 + 0 + 5 + 1 + 0 + 5 = 14
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
        assert_eq!(total, 14);
    }

    #[test]
    fn count_tokens_whitespace_only() {
        let counter = Counter::new();
        // Whitespace-only string is non-empty, so minimum 1 token.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "   "), 1);
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "\t\n"), 1);
    }

    #[test]
    fn count_tokens_multibyte_unicode() {
        let counter = Counter::new();
        // Emoji: U+1F600 is 4 bytes but 1 code point. 1/4 = 0 -> max(0,1) = 1.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "\u{1F600}"), 1);
        // CJK string: 4 characters (each 3 bytes in UTF-8). 4/4 = 1 token.
        assert_eq!(
            counter.count_tokens(HEURISTIC_MODEL, "\u{4F60}\u{597D}\u{4E16}\u{754C}"),
            1
        );
        // 8 CJK characters: 8/4 = 2 tokens.
        let cjk_8: String =
            "\u{4F60}\u{597D}\u{4E16}\u{754C}\u{4F60}\u{597D}\u{4E16}\u{754C}".to_string();
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, &cjk_8), 2);
    }

    #[test]
    fn count_tokens_mixed_ascii_unicode() {
        let counter = Counter::new();
        // "hello" (5 chars) + emoji (1 char) = 6 chars. 6/4 = 1.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, "hello\u{1F600}"), 1);
    }

    // -- BPE dispatch (real tiktoken) ----------------------------------------

    #[test]
    fn count_tokens_unknown_model_uses_heuristic() {
        let counter = Counter::new();
        let text = "The quick brown fox jumps over the lazy dog.";
        let heuristic = (text.chars().count() / 4).max(1);
        // Unknown model -> heuristic.
        assert_eq!(counter.count_tokens(HEURISTIC_MODEL, text), heuristic);
    }

    #[test]
    fn count_tokens_known_model_uses_bpe() {
        let counter = Counter::new();
        let text = "The quick brown fox jumps over the lazy dog.";

        // gpt-4o -> o200k_base. The count must match tiktoken's own encoding of
        // the same text under o200k_base (this verifies dispatch + selection,
        // not a hardcoded magic number).
        let o200k = tiktoken_rs::o200k_base().expect("o200k_base loads");
        assert_eq!(
            counter.count_tokens("gpt-4o", text),
            o200k.encode_with_special_tokens(text).len()
        );

        // gpt-4 -> cl100k_base.
        let cl100k = tiktoken_rs::cl100k_base().expect("cl100k_base loads");
        assert_eq!(
            counter.count_tokens("gpt-4", text),
            cl100k.encode_with_special_tokens(text).len()
        );

        // BPE must differ from the heuristic for this text (otherwise the test
        // would not prove BPE is active).
        let heuristic = (text.chars().count() / 4).max(1);
        assert_ne!(
            counter.count_tokens("gpt-4o", text),
            heuristic,
            "BPE count should differ from the heuristic for this fixture"
        );
    }

    #[test]
    fn count_tokens_bpe_encoding_is_cached() {
        // A second count for the same encoding should reuse the cached BPE
        // (loaded_encodings grows to 1, not 2, and a second distinct encoding
        // grows it to 2).
        let counter = Counter::new();
        let _ = counter.count_tokens("gpt-4o", "hello world");
        let _ = counter.count_tokens("gpt-4o-2024-08-06", "another");
        let loaded = counter.cache.lock().unwrap().len();
        assert_eq!(
            loaded, 1,
            "gpt-4o and gpt-4o-2024-08-06 share the o200k_base encoding"
        );

        let _ = counter.count_tokens("gpt-4", "third");
        let loaded = counter.cache.lock().unwrap().len();
        assert_eq!(loaded, 2, "gpt-4 adds the cl100k_base encoding");
    }
}
