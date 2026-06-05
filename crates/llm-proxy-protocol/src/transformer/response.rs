//! Response transformation: OpenAI / Gemini / Responses API -> Anthropic format.
//!
//! Each public function accepts a provider-specific response and a `model_id`
//! string (the model the caller believes it is talking to) and returns an
//! [`anthropic::MessageResponse`] suitable for streaming back to a Claude Code
//! client.
//!
//! The logic is ported from the Go reference implementation in
//! `ref/oc-go-cc/internal/transformer/response.go`.

use crate::anthropic::{ContentBlock, MessageResponse, Usage};
use crate::openai::{ChatCompletionResponse, UsageInfo};
use crate::zen::{GeminiResponse, ResponsesResponse};
use super::{non_negative, map_finish_reason};

/// Generate a unique response ID using UUID v4.
///
/// UUIDs are deterministic across test runs only when seeded, but are
/// collision-free in concurrent scenarios and suitable for snapshot testing
/// when the generator is injectable.
fn generate_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4())
}

/// Build an empty-text content block (fallback when no other blocks exist).
fn empty_text_block() -> ContentBlock {
    ContentBlock::new_text(String::new())
}

/// Build a tool_use content block from response data (used in response transformers).
fn new_tool_use_response_block(
    id: Option<String>,
    name: Option<String>,
    input: serde_json::Value,
) -> ContentBlock {
    ContentBlock {
        r#type: "tool_use".to_owned(),
        text: None,
        id,
        tool_use_id: None,
        name,
        input: Some(input),
        output: None,
        content: None,
        is_error: None,
        thinking: None,
        signature: None,
        source: None,
    }
}

/// Build a thinking content block from response data.
fn new_thinking_response_block(thinking: String) -> ContentBlock {
    ContentBlock {
        r#type: "thinking".to_owned(),
        thinking: Some(thinking),
        text: None,
        id: None,
        tool_use_id: None,
        name: None,
        input: None,
        output: None,
        content: None,
        is_error: None,
        signature: None,
        source: None,
    }
}

/// Build a text content block from response data.
fn new_text_response_block(text: String) -> ContentBlock {
    ContentBlock {
        r#type: "text".to_owned(),
        text: Some(text),
        id: None,
        tool_use_id: None,
        name: None,
        input: None,
        output: None,
        content: None,
        is_error: None,
        thinking: None,
        signature: None,
        source: None,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Transform an OpenAI [`ChatCompletionResponse`] into an Anthropic
/// [`MessageResponse`].
///
/// Content blocks are built in the following order:
///
/// 1. `reasoning_content` (non-empty) -> `"thinking"` block
/// 2. `tool_calls`                      -> `"tool_use"` blocks
/// 3. `content` (non-empty text)        -> `"text"` block
///
/// If none of the above produce a block, a single empty `"text"` block is
/// emitted so that the response always contains at least one content block.
///
/// Usage mapping accounts for prompt caching:
///
/// ```text
/// input_tokens  = prompt_tokens - cache_hit - cache_miss
/// output_tokens = completion_tokens
/// ```
///
/// The subtraction uses [`non_negative`] to avoid underflow when the
/// upstream reports inconsistent counts.
pub fn transform_response(
    resp: &ChatCompletionResponse,
    model_id: &str,
) -> Result<MessageResponse, String> {
    if resp.choices.is_empty() {
        return Err("no choices in response".to_owned());
    }

    let choice = &resp.choices[0];
    let msg = choice
        .message
        .as_ref()
        .ok_or_else(|| "choice has no message".to_owned())?;

    // -- Content blocks -------------------------------------------------------

    let mut blocks: Vec<ContentBlock> = Vec::new();

    // Reasoning content -> thinking block.
    if let Some(ref reasoning) = msg.reasoning_content {
        if !reasoning.is_empty() {
            blocks.push(new_thinking_response_block(reasoning.clone()));
        }
    }

    // Tool calls -> tool_use blocks.
    for tc in &msg.tool_calls {
        // Legacy behavior: malformed tool_call arguments silently fall back to
        // an empty JSON object. The core protocol migration should propagate
        // the parse error or log a warning instead.
        // TODO: Add tracing::warn! when the protocol crate gains a tracing
        // dependency, or propagate the error via a typed TransformError enum.
        let input_json = tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_ref())
            .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        blocks.push(new_tool_use_response_block(
            tc.id.clone(),
            tc.function.as_ref().and_then(|f| f.name.clone()),
            input_json,
        ));
    }

    // Plain text content.
    if !msg.content.is_empty() {
        blocks.push(new_text_response_block(msg.content.clone()));
    }

    // Guarantee at least one content block.
    if blocks.is_empty() {
        blocks.push(empty_text_block());
    }

    // -- Finish reason --------------------------------------------------------

    let stop_reason = choice
        .finish_reason
        .as_deref()
        .map(|r| map_finish_reason(r).to_owned())
        .unwrap_or_else(|| "end_turn".to_owned());

    // -- Usage ----------------------------------------------------------------

    let usage = build_usage_from_openai(&resp.usage);

    Ok(MessageResponse {
        id: resp.id.clone(),
        r#type: "message".to_owned(),
        role: "assistant".to_owned(),
        content: blocks,
        model: model_id.to_owned(),
        stop_reason: Some(stop_reason),
        stop_sequence: None,
        usage,
    })
}

/// Transform a Responses API [`ResponsesResponse`] into an Anthropic
/// [`MessageResponse`].
///
/// Output items are mapped as follows:
///
/// - `"message"` items with `"output_text"` content -> `"text"` blocks
/// - `"function_call"` items -> `"tool_use"` blocks
///
/// Stop reason is always `"end_turn"` because the Responses API does not
/// expose a granular finish reason.
pub fn transform_responses_response(
    resp: &ResponsesResponse,
    model_id: &str,
) -> Result<MessageResponse, String> {
    if resp.output.is_empty() {
        return Err("no output in response".to_owned());
    }

    let mut blocks: Vec<ContentBlock> = Vec::new();

    for output in &resp.output {
        match output.r#type.as_str() {
            "message" => {
                if let Some(ref content_items) = output.content {
                    for c in content_items {
                        if c.r#type == "output_text" {
                            blocks.push(new_text_response_block(
                                c.text.clone().unwrap_or_default(),
                            ));
                        }
                    }
                }
            }
            "function_call" => {
                // Legacy behavior: malformed function_call arguments silently
                // fall back to an empty JSON object.
                // TODO: Add tracing::warn! when the protocol crate gains a
                // tracing dependency, or propagate the error via a typed
                // TransformError enum.
                let input_json = output
                    .arguments
                    .as_deref()
                    .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                blocks.push(new_tool_use_response_block(
                    output.call_id.clone(),
                    output.name.clone(),
                    input_json,
                ));
            }
            // TODO: Unrecognized Responses API output item types are silently
            // dropped. See the `_ => {}` arm in `transform_user_message` in
            // request.rs for the same known gap.
            _ => {}
        }
    }

    // Guarantee at least one content block.
    if blocks.is_empty() {
        blocks.push(empty_text_block());
    }

    Ok(MessageResponse {
        id: resp.id.clone(),
        r#type: "message".to_owned(),
        role: "assistant".to_owned(),
        content: blocks,
        model: model_id.to_owned(),
        stop_reason: Some("end_turn".to_owned()),
        stop_sequence: None,
        usage: Usage {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    })
}

/// Transform a Gemini [`GeminiResponse`] into an Anthropic [`MessageResponse`].
///
/// The first candidate's content parts are converted to `"text"` blocks.
/// The response ID is synthesised as `"gemini_{timestamp_ns}"` because the
/// Gemini API does not provide a stable identifier.
///
/// Finish reason mapping:
///
/// - `"MAX_TOKENS"` -> `"max_tokens"`
/// - anything else  -> `"end_turn"`
pub fn transform_gemini_response(
    resp: &GeminiResponse,
    model_id: &str,
) -> Result<MessageResponse, String> {
    if resp.candidates.is_empty() {
        return Err("no candidates in response".to_owned());
    }

    let candidate = &resp.candidates[0];

    // -- Content blocks -------------------------------------------------------

    let mut blocks: Vec<ContentBlock> = Vec::new();

    for part in &candidate.content.parts {
        if let Some(ref text) = part.text {
            if !text.is_empty() {
                blocks.push(new_text_response_block(text.clone()));
            }
        }
    }

    // Guarantee at least one content block.
    if blocks.is_empty() {
        blocks.push(empty_text_block());
    }

    // -- Finish reason --------------------------------------------------------

    let stop_reason = match candidate.finish_reason.as_deref().unwrap_or("") {
        "STOP" => "end_turn",
        "MAX_TOKENS" => "max_tokens",
        "SAFETY" => "end_turn",
        "RECITATION" => "end_turn",
        _ => "end_turn",
    };

    // -- Usage ----------------------------------------------------------------

    let usage = resp
        .usage_metadata
        .as_ref()
        .map(|um| Usage {
            input_tokens: um.prompt_token_count,
            output_tokens: um.candidates_token_count,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        })
        .unwrap_or(Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });

    // Synthesise an ID since Gemini does not provide one.
    // Uses UUID v4 for uniqueness and testability (no nanosecond collisions).
    let id = generate_id();

    Ok(MessageResponse {
        id,
        r#type: "message".to_owned(),
        role: "assistant".to_owned(),
        content: blocks,
        model: model_id.to_owned(),
        stop_reason: Some(stop_reason.to_owned()),
        stop_sequence: None,
        usage,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build Anthropic [`Usage`] from an OpenAI [`UsageInfo`], subtracting
/// cache token counts from `input_tokens`.
fn build_usage_from_openai(info: &UsageInfo) -> Usage {
    let prompt = info.prompt_tokens as i64;
    let cache_hit = info.prompt_cache_hit_tokens.unwrap_or(0) as i64;
    let cache_miss = info.prompt_cache_miss_tokens.unwrap_or(0) as i64;

    Usage {
        // Per Anthropic Messages API spec, `input_tokens` is the count of
        // regular input tokens -- i.e. tokens that were neither read from
        // the cache nor written to the cache this turn. OpenAI's
        // `prompt_tokens` is the *total* prompt size including both. When
        // the upstream reports prompt-cache fields we subtract them out so
        // that Claude Code's local context counter does not see an inflated
        // input_tokens on every turn.  Saturating conversion: if the computed
        // value exceeds i32::MAX it is clamped rather than silently truncated.
        input_tokens: i32::try_from(non_negative(prompt - cache_hit - cache_miss))
            .unwrap_or(i32::MAX),
        output_tokens: info.completion_tokens,
        cache_creation_input_tokens: info.prompt_cache_miss_tokens,
        cache_read_input_tokens: info.prompt_cache_hit_tokens,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::{ChatMessage, Choice, FunctionCall, ToolCall};

    // -- non_negative ---------------------------------------------------------

    #[test]
    fn non_negative_clamps_to_zero() {
        assert_eq!(non_negative(-5), 0);
        assert_eq!(non_negative(0), 0);
        assert_eq!(non_negative(42), 42);
    }

    // -- map_finish_reason ----------------------------------------------------

    #[test]
    fn finish_reason_stop() {
        assert_eq!(map_finish_reason("stop"), "end_turn");
    }

    #[test]
    fn finish_reason_length() {
        assert_eq!(map_finish_reason("length"), "max_tokens");
    }

    #[test]
    fn finish_reason_tool_calls() {
        assert_eq!(map_finish_reason("tool_calls"), "tool_use");
    }

    #[test]
    fn finish_reason_tool_use() {
        assert_eq!(map_finish_reason("tool_use"), "tool_use");
    }

    #[test]
    fn finish_reason_content_filter() {
        assert_eq!(map_finish_reason("content_filter"), "end_turn");
    }

    #[test]
    fn finish_reason_unknown() {
        assert_eq!(map_finish_reason("something_else"), "end_turn");
    }

    // -- transform_response ---------------------------------------------------

    #[test]
    fn transform_response_no_choices() {
        let resp = make_openai_response(vec![], make_usage(100, 50, 0, 0));
        let err = transform_response(&resp, "test-model").unwrap_err();
        assert_eq!(err, "no choices in response");
    }

    #[test]
    fn transform_response_text_only() {
        let msg = ChatMessage {
            role: "assistant".to_owned(),
            content: "hello world".to_owned(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
        };
        let resp = make_openai_response(
            vec![Choice {
                index: 0,
                message: Some(msg),
                finish_reason: Some("stop".to_owned()),
                delta: None,
            }],
            make_usage(100, 50, 10, 20),
        );

        let result = transform_response(&resp, "my-model").unwrap();
        assert_eq!(result.id, "chatcmpl-test");
        assert_eq!(result.r#type, "message");
        assert_eq!(result.role, "assistant");
        assert_eq!(result.model, "my-model");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(result.stop_sequence, None);

        // input_tokens = 100 - 10 - 20 = 70
        assert_eq!(result.usage.input_tokens, 70);
        assert_eq!(result.usage.output_tokens, 50);
        assert_eq!(result.usage.cache_creation_input_tokens, Some(20));
        assert_eq!(result.usage.cache_read_input_tokens, Some(10));

        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(result.content[0].text.as_deref(), Some("hello world"));
    }

    #[test]
    fn transform_response_with_thinking_and_tool_use() {
        let msg = ChatMessage {
            role: "assistant".to_owned(),
            content: "result text".to_owned(),
            reasoning_content: Some("let me think...".to_owned()),
            tool_calls: vec![ToolCall {
                index: None,
                id: Some("call_123".to_owned()),
                r#type: Some("function".to_owned()),
                function: Some(FunctionCall {
                    name: Some("get_weather".to_owned()),
                    arguments: Some(r#"{"city":"SF"}"#.to_owned()),
                }),
            }],
            name: None,
            tool_call_id: None,
            cache_control: None,
        };
        let resp = make_openai_response(
            vec![Choice {
                index: 0,
                message: Some(msg),
                finish_reason: Some("tool_calls".to_owned()),
                delta: None,
            }],
            make_usage(200, 80, 0, 0),
        );

        let result = transform_response(&resp, "thinking-model").unwrap();
        assert_eq!(result.stop_reason.as_deref(), Some("tool_use"));

        // Order: thinking, tool_use, text
        assert_eq!(result.content.len(), 3);
        assert_eq!(result.content[0].r#type, "thinking");
        assert_eq!(
            result.content[0].thinking.as_deref(),
            Some("let me think...")
        );
        assert_eq!(result.content[1].r#type, "tool_use");
        assert_eq!(result.content[1].id.as_deref(), Some("call_123"));
        assert_eq!(result.content[1].name.as_deref(), Some("get_weather"));
        assert_eq!(result.content[1].input.as_ref().unwrap()["city"], "SF");
        assert_eq!(result.content[2].r#type, "text");
        assert_eq!(result.content[2].text.as_deref(), Some("result text"));
    }

    #[test]
    fn transform_response_empty_message_gives_empty_text_block() {
        let msg = ChatMessage {
            role: "assistant".to_owned(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
        };
        let resp = make_openai_response(
            vec![Choice {
                index: 0,
                message: Some(msg),
                finish_reason: Some("stop".to_owned()),
                delta: None,
            }],
            make_usage(10, 5, 0, 0),
        );

        let result = transform_response(&resp, "model").unwrap();
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(result.content[0].text.as_deref(), Some(""));
    }

    #[test]
    fn transform_response_cache_subtraction_no_underflow() {
        // prompt_tokens < cache_hit + cache_miss => clamp to 0
        let msg = ChatMessage {
            role: "assistant".to_owned(),
            content: "hi".to_owned(),
            reasoning_content: None,
            tool_calls: vec![],
            name: None,
            tool_call_id: None,
            cache_control: None,
        };
        let resp = make_openai_response(
            vec![Choice {
                index: 0,
                message: Some(msg),
                finish_reason: Some("stop".to_owned()),
                delta: None,
            }],
            make_usage(5, 10, 80, 20),
        );

        let result = transform_response(&resp, "model").unwrap();
        // 5 - 80 - 20 = -95, clamped to 0
        assert_eq!(result.usage.input_tokens, 0);
    }

    // -- transform_responses_response -----------------------------------------

    #[test]
    fn transform_responses_response_no_output() {
        let resp = make_responses_response("resp_1", vec![], 100, 50);
        let err = transform_responses_response(&resp, "model").unwrap_err();
        assert_eq!(err, "no output in response");
    }

    #[test]
    fn transform_responses_response_message_and_function_call() {
        use crate::zen::{ResponsesContent, ResponsesOutput};

        let resp = ResponsesResponse {
            id: "resp_abc".to_owned(),
            object: "response".to_owned(),
            created: 1234567890,
            model: "gpt-4o".to_owned(),
            output: vec![
                ResponsesOutput {
                    r#type: "message".to_owned(),
                    id: Some("msg_1".to_owned()),
                    role: Some("assistant".to_owned()),
                    content: Some(vec![ResponsesContent {
                        r#type: "output_text".to_owned(),
                        text: Some("Here is the answer".to_owned()),
                    }]),
                    call_id: None,
                    name: None,
                    arguments: None,
                },
                ResponsesOutput {
                    r#type: "function_call".to_owned(),
                    id: None,
                    role: None,
                    content: None,
                    call_id: Some("call_42".to_owned()),
                    name: Some("search".to_owned()),
                    arguments: Some(r#"{"query":"rust"}"#.to_owned()),
                },
            ],
            usage: crate::zen::ResponsesUsage {
                input_tokens: 200,
                output_tokens: 100,
            },
        };

        let result = transform_responses_response(&resp, "my-model").unwrap();
        assert_eq!(result.id, "resp_abc");
        assert_eq!(result.model, "my-model");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(result.usage.input_tokens, 200);
        assert_eq!(result.usage.output_tokens, 100);

        assert_eq!(result.content.len(), 2);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(
            result.content[0].text.as_deref(),
            Some("Here is the answer")
        );
        assert_eq!(result.content[1].r#type, "tool_use");
        assert_eq!(result.content[1].id.as_deref(), Some("call_42"));
        assert_eq!(result.content[1].name.as_deref(), Some("search"));
    }

    #[test]
    fn transform_responses_response_empty_output_gives_empty_text() {
        use crate::zen::ResponsesOutput;

        let resp = ResponsesResponse {
            id: "resp_empty".to_owned(),
            object: "response".to_owned(),
            created: 0,
            model: "model".to_owned(),
            output: vec![ResponsesOutput {
                r#type: "unknown_type".to_owned(),
                id: None,
                role: None,
                content: None,
                call_id: None,
                name: None,
                arguments: None,
            }],
            usage: crate::zen::ResponsesUsage {
                input_tokens: 0,
                output_tokens: 0,
            },
        };

        let result = transform_responses_response(&resp, "model").unwrap();
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(result.content[0].text.as_deref(), Some(""));
    }

    // -- transform_gemini_response --------------------------------------------

    #[test]
    fn transform_gemini_response_no_candidates() {
        let resp = GeminiResponse {
            candidates: vec![],
            usage_metadata: None,
        };
        let err = transform_gemini_response(&resp, "model").unwrap_err();
        assert_eq!(err, "no candidates in response");
    }

    #[test]
    fn transform_gemini_response_basic() {
        use crate::zen::{GeminiCandidate, GeminiContent, GeminiPart, GeminiUsage};

        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: "model".to_owned(),
                    parts: vec![
                        GeminiPart {
                            text: Some("Hello ".to_owned()),
                        },
                        GeminiPart {
                            text: Some("world".to_owned()),
                        },
                    ],
                },
                finish_reason: Some("STOP".to_owned()),
            }],
            usage_metadata: Some(GeminiUsage {
                prompt_token_count: 50,
                candidates_token_count: 20,
                total_token_count: 70,
            }),
        };

        let result = transform_gemini_response(&resp, "gemini-model").unwrap();
        assert!(result.id.starts_with("msg_"));
        assert_eq!(result.r#type, "message");
        assert_eq!(result.role, "assistant");
        assert_eq!(result.model, "gemini-model");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(result.usage.input_tokens, 50);
        assert_eq!(result.usage.output_tokens, 20);
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.content[0].text.as_deref(), Some("Hello "));
        assert_eq!(result.content[1].text.as_deref(), Some("world"));
    }

    #[test]
    fn transform_gemini_response_max_tokens() {
        use crate::zen::{GeminiCandidate, GeminiContent, GeminiPart};

        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: "model".to_owned(),
                    parts: vec![GeminiPart {
                        text: Some("truncated".to_owned()),
                    }],
                },
                finish_reason: Some("MAX_TOKENS".to_owned()),
            }],
            usage_metadata: None,
        };

        let result = transform_gemini_response(&resp, "gemini-model").unwrap();
        assert_eq!(result.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(result.usage.input_tokens, 0);
        assert_eq!(result.usage.output_tokens, 0);
    }

    #[test]
    fn transform_gemini_response_empty_parts() {
        use crate::zen::{GeminiCandidate, GeminiContent, GeminiPart};

        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: "model".to_owned(),
                    parts: vec![GeminiPart { text: None }],
                },
                finish_reason: None,
            }],
            usage_metadata: None,
        };

        let result = transform_gemini_response(&resp, "model").unwrap();
        // No non-empty parts -> fallback empty text block
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(result.content[0].text.as_deref(), Some(""));
    }

    // -- test helpers ---------------------------------------------------------

    fn make_usage(prompt: i32, completion: i32, cache_hit: i32, cache_miss: i32) -> UsageInfo {
        UsageInfo {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            // Always set cache fields to match production codepaths where
            // `build_usage_from_openai` always sets these from UsageInfo fields.
            prompt_cache_hit_tokens: Some(cache_hit),
            prompt_cache_miss_tokens: Some(cache_miss),
        }
    }

    fn make_openai_response(choices: Vec<Choice>, usage: UsageInfo) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "chatcmpl-test".to_owned(),
            object: "chat.completion".to_owned(),
            created: 1234567890,
            model: "gpt-4o".to_owned(),
            choices,
            usage,
        }
    }

    fn make_responses_response(
        id: &str,
        output: Vec<crate::zen::ResponsesOutput>,
        input_tokens: i32,
        output_tokens: i32,
    ) -> ResponsesResponse {
        ResponsesResponse {
            id: id.to_owned(),
            object: "response".to_owned(),
            created: 0,
            model: "model".to_owned(),
            output,
            usage: crate::zen::ResponsesUsage {
                input_tokens,
                output_tokens,
            },
        }
    }

    // -- Edge case tests for malformed arguments ---------------------------------

    /// Phase 0: tool_call with malformed JSON arguments falls back to empty JSON object.
    #[test]
    fn transform_response_malformed_tool_call_arguments_fallback_empty_json() {
        use crate::openai::{Choice, FunctionCall, ToolCall};

        let msg = crate::openai::ChatMessage {
            role: "assistant".to_owned(),
            content: "hello".to_owned(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                index: None,
                id: Some("call_1".to_owned()),
                r#type: Some("function".to_owned()),
                function: Some(FunctionCall {
                    name: Some("my_tool".to_owned()),
                    arguments: Some("not valid json{{{".to_owned()),
                }),
            }],
            name: None,
            tool_call_id: None,
            cache_control: None,
        };
        let resp = make_openai_response(
            vec![Choice {
                index: 0,
                message: Some(msg),
                finish_reason: Some("stop".to_owned()),
                delta: None,
            }],
            make_usage(10, 5, 0, 0),
        );
        let result = transform_response(&resp, "gpt-4o").unwrap();
        // The tool_use block should exist with empty JSON object as input.
        let tool_block = result.content.iter().find(|b| b.r#type == "tool_use").unwrap();
        assert_eq!(tool_block.input.as_ref(), Some(&serde_json::json!({})));
    }

    /// Phase 0: Responses API function_call with malformed arguments falls back to empty JSON.
    #[test]
    fn transform_responses_response_malformed_arguments_fallback() {
        let output = vec![crate::zen::ResponsesOutput {
            r#type: "function_call".to_owned(),
            id: Some("fc_1".to_owned()),
            role: None,
            content: None,
            call_id: Some("call_1".to_owned()),
            name: Some("my_tool".to_owned()),
            arguments: Some("invalid json!!!".to_owned()),
        }];
        let resp = make_responses_response("resp_1", output, 10, 5);
        let result = transform_responses_response(&resp, "gpt-4o").unwrap();
        let tool_block = result.content.iter().find(|b| b.r#type == "tool_use").unwrap();
        assert_eq!(tool_block.input.as_ref(), Some(&serde_json::json!({})));
    }

    /// Phase 0: Gemini response with empty-string text parts produces no text block.
    #[test]
    fn transform_gemini_response_empty_text_parts() {
        use crate::zen::{GeminiCandidate, GeminiContent, GeminiPart};

        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: GeminiContent {
                    role: "model".to_owned(),
                    parts: vec![GeminiPart {
                        text: Some(String::new()),
                    }],
                },
                finish_reason: Some("STOP".to_owned()),
            }],
            usage_metadata: Some(crate::zen::GeminiUsage {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
            }),
        };
        let result = transform_gemini_response(&resp, "gemini-2.5-flash").unwrap();
        // Empty text should produce no text block, so the fallback empty block is used.
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].r#type, "text");
        assert_eq!(result.content[0].text.as_deref(), Some(""));
    }
}
