//! Token counter with model-aware BPE dispatch.
//!
//! [`Counter`] is the public front door for token counting. For known OpenAI
//! model ids it dispatches to a real BPE tokenizer (via
//! [`super::tiktoken`]); for everything else it falls back to a character
//! heuristic (~4 chars/token). The BPE ranks are bundled at compile time, so
//! counting is offline-safe and the heuristic is always available as a
//! last resort.
//!
//! The per-message overhead mirrors `tiktoken-rs`'s
//! `num_tokens_from_messages` convention (4 tokens/message for the `gpt-3.5`
//! family, 3 otherwise, plus a trailing +3) so unit-test expectations stay
//! aligned with the upstream tokenizer.

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
    /// The formula follows `tiktoken-rs`'s `num_tokens_from_messages`
    /// convention rather than any separate "Go reference":
    ///
    /// ```text
    /// tokens_per_message = if model.starts_with("gpt-3.5") { 4 } else { 3 }
    /// total = sum over (system-as-one-message + each message) of
    ///           (tokens_per_message + count_tokens(role) + count_tokens(content))
    ///       + 3   // trailing priming tokens
    /// ```
    ///
    /// Each piece is counted with the model-appropriate backend (BPE for known
    /// OpenAI models, heuristic otherwise). The system prompt is folded into
    /// the message loop as a single `"system"`-role message when it is
    /// non-empty, so it contributes `tokens_per_message` exactly the way
    /// tiktoken-rs folds a system message into its iteration. When `system` is
    /// empty no per-message overhead is added for it.
    ///
    /// Note: `system` is a flat string. If the upstream protocol supports
    /// multiple system blocks, the caller must pre-concatenate them.
    /// Role validation is the caller's responsibility.
    pub fn count_messages(&self, model: &str, system: &str, messages: &[MessageContent]) -> usize {
        // tiktoken-rs convention (see `num_tokens_from_messages`):
        //   4 tokens/message for gpt-3.5*, 3 for everything else.
        const GPT_35_PER_MESSAGE: usize = 4;
        const DEFAULT_PER_MESSAGE: usize = 3;
        // Trailing +3: every reply is primed with `<|start|>assistant<|message|>`.
        const TRAILING_TOKENS: usize = 3;

        let tokens_per_message = if model.starts_with("gpt-3.5") {
            GPT_35_PER_MESSAGE
        } else {
            DEFAULT_PER_MESSAGE
        };

        // The system prompt is treated as one message in the loop (matching
        // tiktoken-rs, which iterates system alongside the rest). Empty system
        // contributes nothing -- no bogus per-message overhead for a missing
        // block.
        let system_overhead = if system.is_empty() {
            0
        } else {
            tokens_per_message
                + self.count_tokens(model, "system")
                + self.count_tokens(model, system)
        };

        let message_tokens: usize = messages
            .iter()
            .map(|msg| {
                tokens_per_message
                    + self.count_tokens(model, &msg.role)
                    + self.count_tokens(model, &msg.content)
            })
            .sum();

        system_overhead + message_tokens + TRAILING_TOKENS
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
        // system="test" folded in: per_msg(3) + role("system"=6/4=1) + content("test"=4/4=1) = 5
        // 0 messages, +3 trailing = 5 + 0 + 3 = 8
        let total = counter.count_messages(HEURISTIC_MODEL, "test", &[]);
        assert_eq!(total, 8);
    }

    #[test]
    fn count_messages_empty_system() {
        let counter = Counter::new();
        // Empty system -> no system overhead. 0 messages. +3 trailing = 3.
        let total = counter.count_messages(HEURISTIC_MODEL, "", &[]);
        assert_eq!(total, 3);
    }

    #[test]
    fn count_messages_single_user_message() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "hello")];
        // empty system -> 0
        // msg: per_msg(3) + role("user"=4/4=1) + content("hello"=5/4=1) = 5
        // +3 trailing = 0 + 5 + 3 = 8
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
        assert_eq!(total, 8);
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
        // tokens_per_message = 3 (HEURISTIC_MODEL is not gpt-3.5).
        // system="Be helpful"(10/4=2): per_msg(3) + role("system"=6/4=1) + content(2) = 6
        // msg0: per_msg(3) + role("system"=1) + content="You are a helpful assistant."(27/4=6) = 10
        // msg1: per_msg(3) + role("user"=4/4=1) + content="Hello, how are you?"(20/4=5) = 9
        // msg2: per_msg(3) + role("assistant"=9/4=2) + content="I'm doing well, thank you!"(27/4=6) = 11
        // +3 trailing = 6 + 10 + 9 + 11 + 3 = 39
        assert_eq!(total, 39);
    }

    #[test]
    fn count_messages_with_system_prompt() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "Hi")];
        // system="You are helpful"(15/4=3): per_msg(3) + role("system"=1) + content(3) = 7
        // msg: per_msg(3) + role("user"=4/4=1) + content("Hi"=2/4=0->1) = 5
        // +3 trailing = 7 + 5 + 3 = 15
        let total = counter.count_messages(HEURISTIC_MODEL, "You are helpful", &messages);
        assert_eq!(total, 15);
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
        // empty system -> 0
        // msg: per_msg(3) + role("") + content("hello"=5/4=1) = 3 + 0 + 1 = 4
        // +3 trailing = 0 + 4 + 3 = 7
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
        assert_eq!(total, 7);
    }

    #[test]
    fn count_messages_empty_content() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "")];
        // empty system -> 0
        // msg: per_msg(3) + role("user"=4/4=1) + content("") = 3 + 1 + 0 = 4
        // +3 trailing = 0 + 4 + 3 = 7
        let total = counter.count_messages(HEURISTIC_MODEL, "", &messages);
        assert_eq!(total, 7);
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
            "\u{4F60}\u{597D}\u{4E16}\u{754C}\u{4F60}\u{597D}\u{4E16}\u{754C}".to_owned();
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

    // -- tok-02: golden parity with tiktoken-rs -----------------------------

    /// Fixed multi-message fixture shared with tiktoken-rs's
    /// `num_tokens_from_messages`. The proxy's `count_messages` must match the
    /// upstream tokenizer to within a small tolerance (the plan allows <= 3
    /// absolute; since we use the same BPE and the same overhead convention,
    /// equality should hold exactly when `name` is absent).
    fn golden_messages() -> Vec<MessageContent> {
        vec![
            MessageContent::new(
                "system",
                "You are a helpful assistant that only speaks French.",
            ),
            MessageContent::new("user", "Hello, how are you?"),
            MessageContent::new("assistant", "Parlez-vous francais?"),
        ]
    }

    fn golden_tiktoken_messages() -> Vec<tiktoken_rs::ChatCompletionRequestMessage> {
        use tiktoken_rs::ChatCompletionRequestMessage;
        vec![
            ChatCompletionRequestMessage {
                role: "system".to_owned(),
                content: Some("You are a helpful assistant that only speaks French.".to_owned()),
                name: None,
                function_call: None,
            },
            ChatCompletionRequestMessage {
                role: "user".to_owned(),
                content: Some("Hello, how are you?".to_owned()),
                name: None,
                function_call: None,
            },
            ChatCompletionRequestMessage {
                role: "assistant".to_owned(),
                content: Some("Parlez-vous francais?".to_owned()),
                name: None,
                function_call: None,
            },
        ]
    }

    /// The system prompt passed separately to the proxy must equal the
    /// `content` of the system message tiktoken-rs sees (otherwise the two
    /// formulas would not be comparable).
    const GOLDEN_SYSTEM: &str = "Be concise and correct.";

    #[test]
    fn count_messages_matches_tiktoken_for_gpt4o() {
        let counter = Counter::new();
        let messages = golden_messages();
        let tiktoken_msgs = golden_tiktoken_messages();

        let ours = counter.count_messages("gpt-4o", GOLDEN_SYSTEM, &messages);
        let theirs = tiktoken_rs::num_tokens_from_messages("gpt-4o", &tiktoken_msgs)
            .expect("gpt-4o resolves to a supported chat tokenizer");

        // `num_tokens_from_messages` does not model the separate top-level
        // `system` string the proxy exposes. We account for it by handing the
        // same text to tiktoken-rs as an extra leading system message.
        let mut with_system = golden_tiktoken_messages();
        with_system.insert(
            0,
            tiktoken_rs::ChatCompletionRequestMessage {
                role: "system".to_owned(),
                content: Some(GOLDEN_SYSTEM.to_owned()),
                name: None,
                function_call: None,
            },
        );
        let theirs_with_system = tiktoken_rs::num_tokens_from_messages("gpt-4o", &with_system)
            .expect("gpt-4o resolves to a supported chat tokenizer");

        // Same BPE + same overhead convention -> exact equality is expected;
        // the plan's <= 3 tolerance is the documented safety margin.
        assert!(
            (ours as isize - theirs_with_system as isize).abs() <= 3,
            "gpt-4o mismatch: ours={ours}, tiktoken_baseline_no_system={theirs}, \
             tiktoken_with_system={theirs_with_system}"
        );
        // And the system-less baseline must be strictly less than our value
        // (we added a real system message).
        assert!(
            ours > theirs,
            "gpt-4o: adding a system prompt must increase the count (ours={ours}, baseline={theirs})"
        );
    }

    #[test]
    fn count_messages_matches_tiktoken_for_gpt35_turbo() {
        // gpt-3.5-turbo exercises the `tokens_per_message = 4` branch.
        let counter = Counter::new();
        let messages = golden_messages();

        let mut with_system = golden_tiktoken_messages();
        with_system.insert(
            0,
            tiktoken_rs::ChatCompletionRequestMessage {
                role: "system".to_owned(),
                content: Some(GOLDEN_SYSTEM.to_owned()),
                name: None,
                function_call: None,
            },
        );
        let theirs = tiktoken_rs::num_tokens_from_messages("gpt-3.5-turbo", &with_system)
            .expect("gpt-3.5-turbo resolves to a supported chat tokenizer");
        let ours = counter.count_messages("gpt-3.5-turbo", GOLDEN_SYSTEM, &messages);

        assert!(
            (ours as isize - theirs as isize).abs() <= 3,
            "gpt-3.5-turbo mismatch: ours={ours}, tiktoken={theirs}"
        );
    }

    /// gpt-3.5-turbo uses 4 tokens/message while other cl100k models (e.g.
    /// gpt-4) use 3. A fixture with at least one real message must therefore
    /// count strictly higher for gpt-3.5 than for gpt-4. This directly
    /// exercises the `model.starts_with("gpt-3.5")` branch.
    #[test]
    fn count_messages_gpt35_overhead_is_higher_than_gpt4() {
        let counter = Counter::new();
        let messages = vec![MessageContent::new("user", "Hello, how are you today?")];

        let gpt35 = counter.count_messages("gpt-3.5-turbo", "Be helpful", &messages);
        let gpt4 = counter.count_messages("gpt-4", "Be helpful", &messages);
        // system counts as 1 message + the user message = 2 messages, so gpt-3.5
        // (4/msg) is +2 ahead of gpt-4 (3/msg).
        assert_eq!(
            gpt35 - gpt4,
            2,
            "gpt-3.5 uses 4 tokens/message vs gpt-4's 3; with 2 messages the gap is exactly 2"
        );
    }

    // -- NON-ZST: Counter is Clone + Debug + non-zero-sized ------------------

    #[test]
    fn counter_is_clone_and_debug() {
        let counter = Counter::new();
        let cloned = counter.clone();
        // Debug must not panic and must mention the backend.
        let s = format!("{counter:?}");
        assert!(s.contains("bpe+heuristic"), "debug output: {s}");
        // Cloned counter is independently usable.
        assert!(format!("{cloned:?}").contains("bpe+heuristic"));
    }

    #[test]
    fn counter_is_not_zero_sized() {
        // A ZST counter (e.g. if the cache/heuristic fields were dropped) would
        // be a regression: it would mean the lazily-loaded BPE cache is gone.
        assert!(
            std::mem::size_of::<Counter>() > 0,
            "Counter must not be zero-sized; it carries an Arc<Mutex<HashMap<..>>>"
        );
    }
}
