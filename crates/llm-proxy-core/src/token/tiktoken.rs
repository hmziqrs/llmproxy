//! Real BPE token-counting backends via `tiktoken-rs`.
//!
//! `tiktoken-rs` bundles the OpenAI BPE rank files (`cl100k_base`,
//! `o200k_base`, `p50k_base`, `r50k_base`) at compile time via `include_bytes!`,
//! so there is no network fetch at runtime and the proxy is fully offline-safe.
//! The binary grows ~2-4 MB per encoding; ranks are gzip-compressed and
//! decompressed on first use.
//!
//! [`Counter`](super::Counter) remains the public front door. It dispatches to
//! a [`TiktokenTokenizer`] for known OpenAI model ids and falls back to a
//! [`HeuristicTokenizer`] (~4 chars/token) for everything else, so the
//! `/v1/messages/count_tokens` route never hard-fails on an unfamiliar model.

use std::sync::Arc;

use tiktoken_rs::CoreBPE;

/// Approximate number of characters per token for the heuristic backend.
///
/// Shared with [`super::counter`] so the heuristic path keeps the same
/// approximation the proxy has always used.
pub(crate) const CHARS_PER_TOKEN: usize = 4;

/// Token-counting backend.
///
/// `Debug` is required so that [`super::Counter`] (which may hold
/// `Arc<TiktokenTokenizer>`) can be formatted.
pub trait Tokenizer: Send + Sync + std::fmt::Debug {
    /// Count tokens in a single text string.
    fn count_tokens(&self, text: &str) -> usize;
}

/// Heuristic backend (~4 chars/token). Zero-allocation and infallible.
///
/// Used as the fallback for unknown models and whenever a BPE encoding cannot
/// be loaded. Counts Unicode code points (not bytes) for better accuracy with
/// multi-byte scripts (CJK, emoji).
#[derive(Debug, Clone, Default)]
pub struct HeuristicTokenizer;

impl Tokenizer for HeuristicTokenizer {
    fn count_tokens(&self, text: &str) -> usize {
        heuristic_count(text)
    }
}

/// Heuristic token estimate: ~`CHARS_PER_TOKEN` characters per token, counting
/// Unicode code points. Returns 0 for empty input and a minimum of 1 for any
/// non-empty string.
pub(crate) fn heuristic_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    (text.chars().count() / CHARS_PER_TOKEN).max(1)
}

/// BPE backend using a specific tiktoken encoding.
///
/// `CoreBPE` is `Send + Sync` in `tiktoken-rs` 0.6 (its fields are
/// `HashMap`/`Vec<Regex>`). Wrapped in `Arc` so cloning the tokenizer is a
/// refcount bump.
#[derive(Clone)]
pub struct TiktokenTokenizer {
    bpe: Arc<CoreBPE>,
}

impl std::fmt::Debug for TiktokenTokenizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not dump the (large) rank tables; the encoding identity is what
        // matters for diagnostics.
        f.debug_struct("TiktokenTokenizer").finish_non_exhaustive()
    }
}

impl TiktokenTokenizer {
    /// Wrap an already-loaded BPE.
    pub(crate) fn new(bpe: Arc<CoreBPE>) -> Self {
        Self { bpe }
    }
}

impl Tokenizer for TiktokenTokenizer {
    fn count_tokens(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        // `encode_with_special_tokens` is the counting convention used by
        // tiktoken-rs's own message-token counter. For typical input that does
        // not contain literal special-token strings it is equivalent to
        // `encode_ordinary`. It is infallible.
        self.bpe.encode_with_special_tokens(text).len()
    }
}

/// A tiktoken BPE encoding family.
///
/// These are the four OpenAI encodings whose rank tables are bundled into the
/// binary at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// `o200k_base` -- `gpt-4o`, `o1-*`, `o3-*`.
    O200kBase,
    /// `cl100k_base` -- `gpt-4`, `gpt-3.5-turbo`, and their derivatives.
    Cl100kBase,
    /// `p50k_base` -- legacy text/code models (`text-davinci-003`, `code-*`).
    P50kBase,
    /// `r50k_base` -- base (`gpt-3`-era) models.
    R50kBase,
}

impl Encoding {
    /// Load the BPE ranks for this encoding.
    ///
    /// Ranks are bundled at compile time, so this is effectively infallible in
    /// practice; the `Result` is handled defensively. Returns `None` (after a
    /// one-line `warn!`) if the ranks somehow cannot be loaded, so the caller
    /// can fall back to the heuristic rather than failing the request.
    pub(crate) fn load(self) -> Option<Arc<CoreBPE>> {
        let result = match self {
            Encoding::O200kBase => tiktoken_rs::o200k_base(),
            Encoding::Cl100kBase => tiktoken_rs::cl100k_base(),
            Encoding::P50kBase => tiktoken_rs::p50k_base(),
            Encoding::R50kBase => tiktoken_rs::r50k_base(),
        };
        match result {
            Ok(bpe) => Some(Arc::new(bpe)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    encoding = ?self,
                    "failed to load tiktoken BPE ranks; falling back to heuristic token counting"
                );
                None
            }
        }
    }
}

/// Select the tiktoken encoding for a model id.
///
/// Mirrors OpenAI's `encoding_for_model` mapping with longest-prefix matching.
/// Prefix order matters: `gpt-4o` is checked before `gpt-4` (both start with
/// `gpt-4`) so the `o200k_base` family wins for `gpt-4o*`. Returns `None` for
/// unknown models so the caller can use the heuristic.
///
/// Non-OpenAI providers (Anthropic, Gemini, Fireworks, ...) do not publish
/// tiktoken-compatible tokenizers; they fall through to `None` here. For those,
/// provider-reported [`Usage`](llm_proxy_protocol::core::Usage) remains the
/// authoritative count for billing; the local counter is only an estimate for
/// the `/v1/messages/count_tokens` route and pre-flight guardrails.
pub fn encoding_for_model(model: &str) -> Option<Encoding> {
    // `o200k_base` family.
    if model.starts_with("gpt-4o")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
    {
        return Some(Encoding::O200kBase);
    }
    // `cl100k_base` family.
    if model.starts_with("gpt-4") || model.starts_with("gpt-3.5") || model.starts_with("gpt-35") {
        return Some(Encoding::Cl100kBase);
    }
    // `p50k_base` family (legacy text/code models).
    if model.starts_with("text-davinci-003")
        || model.starts_with("text-davinci-002")
        || model.starts_with("code-")
        || model.starts_with("text-davinci")
        || model.starts_with("davinci")
    {
        return Some(Encoding::P50kBase);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_for_model_maps_known_families() {
        // o200k_base family.
        assert_eq!(encoding_for_model("gpt-4o"), Some(Encoding::O200kBase));
        assert_eq!(
            encoding_for_model("gpt-4o-2024-08-06"),
            Some(Encoding::O200kBase)
        );
        assert_eq!(encoding_for_model("o1-preview"), Some(Encoding::O200kBase));
        assert_eq!(encoding_for_model("o1-mini"), Some(Encoding::O200kBase));
        assert_eq!(encoding_for_model("o3-mini"), Some(Encoding::O200kBase));
        // cl100k_base family. Note: must NOT match the gpt-4o prefix.
        assert_eq!(encoding_for_model("gpt-4"), Some(Encoding::Cl100kBase));
        assert_eq!(
            encoding_for_model("gpt-4-turbo"),
            Some(Encoding::Cl100kBase)
        );
        assert_eq!(
            encoding_for_model("gpt-3.5-turbo"),
            Some(Encoding::Cl100kBase)
        );
        // p50k_base family.
        assert_eq!(
            encoding_for_model("text-davinci-003"),
            Some(Encoding::P50kBase)
        );
        assert_eq!(
            encoding_for_model("code-davinci-002"),
            Some(Encoding::P50kBase)
        );
        // Unknown -> None (heuristic).
        assert_eq!(encoding_for_model("claude-sonnet-4-6"), None);
        assert_eq!(encoding_for_model("gemini-2.5-pro"), None);
        assert_eq!(
            encoding_for_model("accounts/fireworks/models/deepseek-v3"),
            None
        );
    }

    #[test]
    fn heuristic_count_basic() {
        assert_eq!(heuristic_count(""), 0);
        assert_eq!(heuristic_count("a"), 1);
        assert_eq!(heuristic_count("hello"), 1); // 5/4 = 1
        assert_eq!(heuristic_count(&"a".repeat(20)), 5); // 20/4 = 5
        // CJK: 4 code points -> 1 token.
        assert_eq!(heuristic_count("\u{4F60}\u{597D}\u{4E16}\u{754C}"), 1);
    }
}
