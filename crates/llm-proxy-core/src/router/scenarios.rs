//! Scenario detection for model routing.
//!
//! Ported from `internal/router/scenarios.go` in the Go reference implementation.
//!
//! Routing priority (detect scenario):
//! 1. **Long Context** -- token count exceeds configurable threshold (default 100K).
//! 2. **Complex** -- architectural keywords or tool-heavy operations detected.
//! 3. **Think** -- reasoning / thinking patterns detected.
//! 4. **Background** -- truly simple read-only operations with no tool keywords.
//! 5. **Default** -- fallback.
//!
//! For streaming requests, use [`route_for_streaming`] which prefers faster models
//! for lower time-to-first-token (TTFT).

use crate::token::counter::MessageContent;

/// Default long-context token threshold (100K tokens).
pub const DEFAULT_LONG_CONTEXT_THRESHOLD: i32 = 100_000;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Scenario represents the routing scenario for model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scenario {
    /// Default fallback scenario.
    Default,
    /// Truly simple background tasks (read-only, no tools).
    Background,
    /// Reasoning / thinking patterns detected.
    Think,
    /// Complex or tool-based operations detected.
    Complex,
    /// Token count exceeds the long-context threshold.
    LongContext,
    /// Fast model for streaming (downgraded from complex/think for better TTFT).
    Fast,
}

impl std::fmt::Display for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scenario::Default => write!(f, "default"),
            Scenario::Background => write!(f, "background"),
            Scenario::Think => write!(f, "think"),
            Scenario::Complex => write!(f, "complex"),
            Scenario::LongContext => write!(f, "long_context"),
            Scenario::Fast => write!(f, "fast"),
        }
    }
}

/// Result of scenario detection containing the chosen scenario and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioResult {
    /// The detected scenario.
    pub scenario: Scenario,
    /// Token count of the request.
    pub token_count: i32,
    /// Human-readable reason for the routing decision.
    pub reason: String,
}

/// Configuration for scenario detection.
///
/// For now this is a simple struct. It can be extended or replaced with a
/// reference to the full [`crate::Config`] when the config types grow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioConfig {
    /// Token count threshold above which the long-context scenario is triggered.
    pub context_threshold: i32,
    /// Optional model ID to mention in streaming long-context reason strings.
    pub long_context_model_id: Option<String>,
}

impl Default for ScenarioConfig {
    fn default() -> Self {
        Self {
            context_threshold: DEFAULT_LONG_CONTEXT_THRESHOLD,
            long_context_model_id: None,
        }
    }
}

impl ScenarioConfig {
    /// Create a new config with the given threshold.
    pub fn new(threshold: i32) -> Self {
        Self {
            context_threshold: threshold,
            long_context_model_id: None,
        }
    }

    /// Create a new config with threshold and model ID.
    pub fn with_model(threshold: i32, model_id: impl Into<String>) -> Self {
        Self {
            context_threshold: threshold,
            long_context_model_id: Some(model_id.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Analyzes a request to determine which model to use.
///
/// Routing priority:
/// 1. Long Context (token count > threshold)
/// 2. Complex (architectural patterns or tool-heavy operations)
/// 3. Think (reasoning patterns)
/// 4. Background (simple operations with NO tools)
/// 5. Default
///
/// For streaming requests, consider using [`route_for_streaming`] to prefer
/// faster models.
pub fn detect_scenario(
    messages: &[MessageContent],
    token_count: i32,
    config: Option<&ScenarioConfig>,
) -> ScenarioResult {
    // 1. Long context takes highest priority.
    let threshold = get_long_context_threshold(config);
    if token_count > threshold {
        return ScenarioResult {
            scenario: Scenario::LongContext,
            token_count,
            reason: format!(
                "token count {token_count} exceeds threshold {threshold} (use MiniMax for 1M context)"
            ),
        };
    }

    // 2. Complex or tool-based operations.
    if has_complex_pattern(messages) {
        return ScenarioResult {
            scenario: Scenario::Complex,
            token_count,
            reason: "complex or tool-based operation detected (use GLM-5.1)".to_string(),
        };
    }

    // 3. Thinking / reasoning patterns.
    if has_thinking_pattern(messages) {
        return ScenarioResult {
            scenario: Scenario::Think,
            token_count,
            reason: "thinking/reasoning pattern detected (use GLM-5)".to_string(),
        };
    }

    // 4. Truly simple background tasks.
    if has_background_pattern(messages) {
        return ScenarioResult {
            scenario: Scenario::Background,
            token_count,
            reason: "simple background task detected (use Qwen3.5 Plus)".to_string(),
        };
    }

    // 5. Default fallback.
    ScenarioResult {
        scenario: Scenario::Default,
        token_count,
        reason: "default scenario (use Kimi K2.6)".to_string(),
    }
}

/// Selects a model optimized for streaming latency.
///
/// For streaming, we prioritize fast TTFT (time-to-first-token) over capability.
/// This may return a less capable model but one that streams faster.
pub fn route_for_streaming(
    messages: &[MessageContent],
    token_count: i32,
    config: Option<&ScenarioConfig>,
) -> ScenarioResult {
    let threshold = get_long_context_threshold(config);

    // Long context always needs the big model regardless of streaming.
    if token_count > threshold {
        let model = config
            .and_then(|c| c.long_context_model_id.as_deref())
            .unwrap_or("long_context");
        return ScenarioResult {
            scenario: Scenario::LongContext,
            token_count,
            reason: format!(
                "high token count streaming ({token_count} > {threshold}) - use {model} for acceptable TTFT"
            ),
        };
    }

    // Complex or thinking requests are downgraded to a fast model for streaming.
    if has_complex_pattern(messages) || has_thinking_pattern(messages) {
        return ScenarioResult {
            scenario: Scenario::Fast,
            token_count,
            reason: "complex request but streaming - use fast model (qwen3.6-plus) for better TTFT"
                .to_string(),
        };
    }

    // Default to fast scenario for streaming.
    ScenarioResult {
        scenario: Scenario::Fast,
        token_count,
        reason: "streaming request - use fast model (qwen3.6-plus)".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true when complex / tool-based patterns are found in system or user
/// messages.
fn has_complex_pattern(messages: &[MessageContent]) -> bool {
    static KEYWORDS: &[&str] = &[
        // Architectural
        "architect",
        "architecture",
        "refactor",
        "redesign",
        "complex",
        "difficult",
        "challenging",
        "optimize",
        "performance",
        "efficiency",
        "design pattern",
        "best practice",
        // Tool-related
        "execute",
        "run command",
        "bash",
        "shell",
        "implement",
        "build",
        "create",
        "add feature",
        "write to",
        "edit file",
        "create file",
    ];

    for msg in messages {
        if msg.role == "system" || msg.role == "user" {
            let lower = msg.content.to_ascii_lowercase();
            for kw in KEYWORDS {
                if lower.contains(kw) {
                    return true;
                }
            }
        }
    }
    false
}

/// Returns true when thinking / reasoning patterns are found in system or user
/// messages, or when an Anthropic thinking block marker is present in any
/// message.
fn has_thinking_pattern(messages: &[MessageContent]) -> bool {
    static KEYWORDS: &[&str] = &[
        "think",
        "thinking",
        "plan",
        "reason",
        "reasoning",
        "analyze",
        "analysis",
        "step by step",
    ];

    for msg in messages {
        // Keywords are checked only for system and user roles.
        if msg.role == "system" || msg.role == "user" {
            let lower = msg.content.to_ascii_lowercase();
            for kw in KEYWORDS {
                if lower.contains(kw) {
                    return true;
                }
            }
        }
        // Anthropic thinking blocks can appear in any role.
        if msg.content.contains("antThinking") {
            return true;
        }
    }
    false
}

/// Returns true only for truly simple, read-only operations with **no** tool
/// keywords present.
///
/// This is intentionally conservative -- when in doubt it returns `false`.
fn has_background_pattern(messages: &[MessageContent]) -> bool {
    // Tool-blocker keywords: if ANY of these appear, it is NOT background.
    static TOOL_BLOCKERS: &[&str] = &[
        "tool",
        "function",
        "execute",
        "run command",
        "write",
        "edit",
        "create",
        "delete",
        "remove",
        "implement",
        "build",
        "add",
        "modify",
    ];

    // Simple background keywords.
    static BACKGROUND_KEYWORDS: &[&str] = &[
        "list directory",
        "ls -",
        "dir",
        "show file",
        "view file",
        "cat file",
        "what is",
        "what's",
        "tell me about",
        "check status",
        "show status",
    ];

    // If any tool keyword appears in any message, immediately reject.
    for msg in messages {
        let lower = msg.content.to_ascii_lowercase();
        for kw in TOOL_BLOCKERS {
            if lower.contains(kw) {
                return false;
            }
        }
    }

    // Check for background keywords.
    for msg in messages {
        let lower = msg.content.to_ascii_lowercase();
        for kw in BACKGROUND_KEYWORDS {
            if lower.contains(kw) {
                return true;
            }
        }
    }
    false
}

/// Returns the configured long-context threshold, or the default (100K).
fn get_long_context_threshold(config: Option<&ScenarioConfig>) -> i32 {
    match config {
        Some(cfg) if cfg.context_threshold > 0 => cfg.context_threshold,
        _ => DEFAULT_LONG_CONTEXT_THRESHOLD,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- has_complex_pattern --------------------------------------------------

    #[test]
    fn has_complex_pattern_user_message() {
        let messages = vec![MessageContent::new(
            "user",
            "Please refactor this code to use interfaces",
        )];
        assert!(has_complex_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_system_message() {
        let messages = vec![MessageContent::new(
            "system",
            "Please architect the new service",
        )];
        assert!(has_complex_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_no_match() {
        let messages = vec![MessageContent::new("user", "Hello, how are you?")];
        assert!(!has_complex_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_bash_keyword() {
        let messages = vec![MessageContent::new("user", "Run this in bash please")];
        assert!(has_complex_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_edit_file_keyword() {
        let messages = vec![MessageContent::new("user", "Please edit file main.rs")];
        assert!(has_complex_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_ignores_assistant_role() {
        let messages = vec![MessageContent::new(
            "assistant",
            "I will refactor the code now",
        )];
        assert!(!has_complex_pattern(&messages));
    }

    // -- has_thinking_pattern --------------------------------------------------

    #[test]
    fn has_thinking_pattern_user_message() {
        let messages = vec![MessageContent::new(
            "user",
            "Think through this problem step by step",
        )];
        assert!(has_thinking_pattern(&messages));
    }

    #[test]
    fn has_thinking_pattern_system_message() {
        let messages = vec![MessageContent::new("system", "You are a reasoning agent")];
        assert!(has_thinking_pattern(&messages));
    }

    #[test]
    fn has_thinking_pattern_anthropic_thinking_block() {
        let messages = vec![MessageContent::new(
            "user",
            "Solve this problem antThinking(thinking block)",
        )];
        assert!(has_thinking_pattern(&messages));
    }

    #[test]
    fn has_thinking_pattern_analyze_keyword() {
        let messages = vec![MessageContent::new(
            "user",
            "Analyze the tradeoffs of this design",
        )];
        assert!(has_thinking_pattern(&messages));
    }

    #[test]
    fn has_thinking_pattern_no_match() {
        let messages = vec![MessageContent::new("user", "Hello, how are you?")];
        assert!(!has_thinking_pattern(&messages));
    }

    // -- has_background_pattern -----------------------------------------------

    #[test]
    fn has_background_pattern_list_directory() {
        let messages = vec![MessageContent::new("user", "list directory contents")];
        assert!(has_background_pattern(&messages));
    }

    #[test]
    fn has_background_pattern_what_is() {
        let messages = vec![MessageContent::new(
            "user",
            "what is the capital of France?",
        )];
        assert!(has_background_pattern(&messages));
    }

    #[test]
    fn has_background_pattern_blocked_by_tool_keyword() {
        let messages = vec![MessageContent::new(
            "user",
            "list directory and create a new file",
        )];
        assert!(!has_background_pattern(&messages));
    }

    #[test]
    fn has_background_pattern_blocked_by_edit_keyword() {
        let messages = vec![MessageContent::new("user", "show file and edit it")];
        assert!(!has_background_pattern(&messages));
    }

    #[test]
    fn has_background_pattern_no_match() {
        let messages = vec![MessageContent::new("user", "Hello, how are you?")];
        assert!(!has_background_pattern(&messages));
    }

    // -- detect_scenario -------------------------------------------------------

    fn mock_config() -> ScenarioConfig {
        ScenarioConfig::new(60_000)
    }

    #[test]
    fn detect_scenario_complex_from_user() {
        let messages = vec![MessageContent::new(
            "user",
            "Architect a new microservice for user authentication",
        )];
        let result = detect_scenario(&messages, 100, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Complex);
    }

    #[test]
    fn detect_scenario_think_from_user() {
        let messages = vec![MessageContent::new(
            "user",
            "Analyze the tradeoffs of this design",
        )];
        let result = detect_scenario(&messages, 100, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Think);
    }

    #[test]
    fn detect_scenario_default_from_simple_user_message() {
        let messages = vec![MessageContent::new("user", "Hello, how are you?")];
        let result = detect_scenario(&messages, 100, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Default);
    }

    #[test]
    fn detect_scenario_long_context_takes_priority() {
        let messages = vec![MessageContent::new("user", "Refactor this code")];
        // Token count > 60_000 triggers long_context regardless of content.
        let result = detect_scenario(&messages, 70_000, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::LongContext);
        assert_eq!(result.token_count, 70_000);
        assert!(
            result.reason.contains("70000"),
            "reason should mention token count: {}",
            result.reason
        );
        assert!(
            result.reason.contains("60000"),
            "reason should mention threshold: {}",
            result.reason
        );
    }

    #[test]
    fn detect_scenario_background_simple_query() {
        let messages = vec![MessageContent::new("user", "what is the capital of Japan?")];
        let result = detect_scenario(&messages, 50, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Background);
    }

    #[test]
    fn detect_scenario_none_config_uses_default_threshold() {
        let messages = vec![MessageContent::new("user", "Hello")];
        // Below default 100K threshold.
        let result = detect_scenario(&messages, 90_000, None);
        assert_eq!(result.scenario, Scenario::Default);

        // Above default 100K threshold.
        let result = detect_scenario(&messages, 110_000, None);
        assert_eq!(result.scenario, Scenario::LongContext);
    }

    #[test]
    fn detect_scenario_complex_beats_think() {
        // "refactor" matches complex before think is checked.
        let messages = vec![MessageContent::new(
            "user",
            "Please refactor this and think about it",
        )];
        let result = detect_scenario(&messages, 100, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Complex);
    }

    #[test]
    fn detect_scenario_think_beats_background() {
        // "analyze" matches think before background is checked.
        let messages = vec![MessageContent::new(
            "user",
            "Analyze this: what is the status?",
        )];
        let result = detect_scenario(&messages, 100, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Think);
    }

    // -- route_for_streaming ---------------------------------------------------

    #[test]
    fn route_for_streaming_respects_configured_threshold() {
        let messages = vec![MessageContent::new("user", "Hello")];
        let cfg = ScenarioConfig::with_model(256_000, "deepseek-v4-flash");

        // Below threshold should NOT trigger long_context.
        let result = route_for_streaming(&messages, 40_955, Some(&cfg));
        assert_ne!(result.scenario, Scenario::LongContext);

        // Above threshold should trigger long_context.
        let result = route_for_streaming(&messages, 300_000, Some(&cfg));
        assert_eq!(result.scenario, Scenario::LongContext);
        assert!(
            result.reason.contains("deepseek-v4-flash"),
            "reason should mention configured model: {}",
            result.reason
        );
    }

    #[test]
    fn route_for_streaming_uses_default_threshold_when_not_configured() {
        let messages = vec![MessageContent::new("user", "Hello")];
        let cfg = ScenarioConfig::default();

        // Below default 100K threshold.
        let result = route_for_streaming(&messages, 90_000, Some(&cfg));
        assert_ne!(result.scenario, Scenario::LongContext);

        // Above default 100K threshold.
        let result = route_for_streaming(&messages, 110_000, Some(&cfg));
        assert_eq!(result.scenario, Scenario::LongContext);
    }

    #[test]
    fn route_for_streaming_nil_config() {
        let messages = vec![MessageContent::new("user", "Hello")];

        // Below default 100K threshold with no config.
        let result = route_for_streaming(&messages, 90_000, None);
        assert_ne!(result.scenario, Scenario::LongContext);

        // Above default 100K threshold with no config.
        let result = route_for_streaming(&messages, 110_000, None);
        assert_eq!(result.scenario, Scenario::LongContext);
        assert!(
            result.reason.contains("long_context"),
            "reason should contain fallback model name: {}",
            result.reason
        );
    }

    #[test]
    fn route_for_streaming_complex_downgrades_to_fast() {
        let messages = vec![MessageContent::new(
            "user",
            "Implement a new feature for user auth",
        )];
        let result = route_for_streaming(&messages, 100, None);
        assert_eq!(result.scenario, Scenario::Fast);
    }

    #[test]
    fn route_for_streaming_thinking_downgrades_to_fast() {
        let messages = vec![MessageContent::new(
            "user",
            "Think carefully about this problem",
        )];
        let result = route_for_streaming(&messages, 100, None);
        assert_eq!(result.scenario, Scenario::Fast);
    }

    #[test]
    fn route_for_streaming_default_is_fast() {
        let messages = vec![MessageContent::new("user", "Hello, how are you?")];
        let result = route_for_streaming(&messages, 100, None);
        assert_eq!(result.scenario, Scenario::Fast);
    }

    // -- edge cases / additional coverage --------------------------------------

    #[test]
    fn scenario_display_snake_case() {
        assert_eq!(Scenario::LongContext.to_string(), "long_context");
        assert_eq!(Scenario::Default.to_string(), "default");
        assert_eq!(Scenario::Background.to_string(), "background");
        assert_eq!(Scenario::Think.to_string(), "think");
        assert_eq!(Scenario::Complex.to_string(), "complex");
        assert_eq!(Scenario::Fast.to_string(), "fast");
    }

    #[test]
    fn scenario_config_default_threshold() {
        let cfg = ScenarioConfig::default();
        assert_eq!(cfg.context_threshold, 100_000);
        assert!(cfg.long_context_model_id.is_none());
    }

    #[test]
    fn scenario_config_with_model() {
        let cfg = ScenarioConfig::with_model(200_000, "minimax-01");
        assert_eq!(cfg.context_threshold, 200_000);
        assert_eq!(cfg.long_context_model_id.as_deref(), Some("minimax-01"));
    }

    #[test]
    fn detect_scenario_multiple_messages() {
        let messages = vec![
            MessageContent::new("system", "You are a helpful assistant"),
            MessageContent::new("user", "Please build a REST API"),
        ];
        let result = detect_scenario(&messages, 500, Some(&mock_config()));
        assert_eq!(result.scenario, Scenario::Complex);
    }

    #[test]
    fn detect_scenario_empty_messages() {
        let result = detect_scenario(&[], 0, None);
        assert_eq!(result.scenario, Scenario::Default);
    }

    #[test]
    fn has_background_pattern_check_status() {
        let messages = vec![MessageContent::new("user", "check status of the server")];
        assert!(has_background_pattern(&messages));
    }

    #[test]
    fn has_background_pattern_show_file() {
        let messages = vec![MessageContent::new(
            "user",
            "show file contents of config.yaml",
        )];
        assert!(has_background_pattern(&messages));
    }

    #[test]
    fn has_complex_pattern_implement() {
        let messages = vec![MessageContent::new("user", "implement the auth module")];
        assert!(has_complex_pattern(&messages));
    }

    #[test]
    fn route_for_streaming_long_context_with_model_id() {
        let messages = vec![MessageContent::new("user", "Hello")];
        let cfg = ScenarioConfig::with_model(50_000, "minimax-01");
        let result = route_for_streaming(&messages, 60_000, Some(&cfg));
        assert_eq!(result.scenario, Scenario::LongContext);
        assert!(result.reason.contains("minimax-01"));
    }
}
