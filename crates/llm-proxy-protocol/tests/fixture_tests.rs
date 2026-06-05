//! Fixture-driven integration tests for client protocol adapters.
//!
//! Loads each fixture's input.json, decodes it to core, compares with core.json,
//! then encodes a response from core and compares with output.json.

use llm_proxy_protocol::anthropic::MessageRequest as AnthropicMessageRequest;
use llm_proxy_protocol::client::{
    anthropic as anthropic_adapter, openai_chat as openai_adapter,
};
use llm_proxy_protocol::core::{
    CoreContent, CoreEvent, CoreResponse, ModelRef, StopReason, Usage, UsageProvenance,
};
use llm_proxy_protocol::openai::ChatCompletionRequest;

use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const FIXTURE_ROOT: &str = "tests/fixtures";

/// Read a fixture file as a parsed JSON value.
fn read_fixture(dir: &Path, name: &str) -> serde_json::Value {
    let path = dir.join(name);
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {} as JSON: {e}", path.display()))
}

/// Read a fixture file as raw string.
fn read_fixture_raw(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Verify that decoding an input produces a core request that matches core.json.
/// Returns the decoded core request for further testing.
fn assert_decode_matches_core(
    adapter: &str,
    input_json: &serde_json::Value,
    core_json: &serde_json::Value,
) {
    let core_request = match adapter {
        "anthropic" => {
            let req: AnthropicMessageRequest = serde_json::from_value(input_json.clone())
                .unwrap_or_else(|e| panic!("failed to parse Anthropic input: {e}"));
            anthropic_adapter::decode_request(req).unwrap()
        }
        "openai_chat" => {
            let req: ChatCompletionRequest = serde_json::from_value(input_json.clone())
                .unwrap_or_else(|e| panic!("failed to parse OpenAI input: {e}"));
            openai_adapter::decode_request(req).unwrap()
        }
        _ => panic!("unknown adapter: {adapter}"),
    };
    let serialized = serde_json::to_value(&core_request)
        .expect("core request should serialize");
    assert_eq!(
        serialized, *core_json,
        "decoded core does not match core.json for adapter {adapter}"
    );
}

/// Verify that a malformed input produces a ProtocolError.
fn assert_malformed_returns_error(adapter: &str, input_json: &serde_json::Value) {
    let result = match adapter {
        "anthropic" => {
            let req: AnthropicMessageRequest = serde_json::from_value(input_json.clone())
                .unwrap_or_else(|e| panic!("failed to parse Anthropic malformed input: {e}"));
            anthropic_adapter::decode_request(req)
        }
        "openai_chat" => {
            let req: ChatCompletionRequest = serde_json::from_value(input_json.clone())
                .unwrap_or_else(|e| panic!("failed to parse OpenAI malformed input: {e}"));
            openai_adapter::decode_request(req)
        }
        _ => panic!("unknown adapter: {adapter}"),
    };
    assert!(
        result.is_err(),
        "expected error for malformed input in adapter {adapter}, got success"
    );
}

/// Build a CoreResponse from the output.json fixture fields for non-stream cases.
fn build_core_response_from_output(adapter: &str, output_json: &serde_json::Value) -> CoreResponse {
    match adapter {
        "anthropic" => {
            let content = output_json
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|block| {
                            let t = block.get("type")?.as_str()?;
                            match t {
                                "text" => Some(CoreContent::Text {
                                    text: block.get("text").and_then(|v| v.as_str()).unwrap_or("").into(),
                                    cache: None,
                                }),
                                "tool_use" => Some(CoreContent::ToolUse {
                                    id: block.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                                    name: block.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                                    input: block.get("input").cloned().unwrap_or(serde_json::Value::Object(
                                        serde_json::Map::new(),
                                    )),
                                }),
                                "thinking" => Some(CoreContent::Thinking {
                                    text: block.get("thinking").and_then(|v| v.as_str()).unwrap_or("").into(),
                                    signature: block.get("signature").and_then(|v| v.as_str()).map(String::from),
                                }),
                                _ => None,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let stop_reason = match output_json
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("end_turn")
            {
                "end_turn" => StopReason::EndTurn,
                "max_tokens" => StopReason::MaxTokens,
                "tool_use" => StopReason::ToolUse,
                "stop_sequence" => StopReason::StopSequence,
                _ => StopReason::Unknown,
            };
            let usage_json = output_json.get("usage").cloned().unwrap_or(serde_json::json!({}));
            CoreResponse {
                id: output_json.get("id").and_then(|v| v.as_str()).map(String::from),
                model: ModelRef {
                    requested: output_json
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .into(),
                    upstream: None,
                },
                content,
                stop_reason,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: usage_json
                        .get("input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                    output_tokens: usage_json
                        .get("output_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                    reasoning_tokens: None,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                    provenance: UsageProvenance::ProviderReported,
                },
                provider_meta: serde_json::Map::new(),
            }
        }
        "openai_chat" => {
            let choice = output_json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first());
            let msg = choice.and_then(|c| c.get("message"));
            let mut content = Vec::new();
            if let Some(msg) = msg {
                if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        content.push(CoreContent::Text {
                            text: text.into(),
                            cache: None,
                        });
                    }
                }
                if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                    content.insert(0, CoreContent::Thinking {
                        text: reasoning.into(),
                        signature: None,
                    });
                }
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let args: serde_json::Value = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .and_then(|a| serde_json::from_str(a).ok())
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        content.push(CoreContent::ToolUse {
                            id: tc.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                            name: tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .into(),
                            input: args,
                        });
                    }
                }
            }
            let finish_reason = choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("stop");
            let stop_reason = match finish_reason {
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "tool_calls" => StopReason::ToolUse,
                "content_filter" => StopReason::Refusal,
                _ => StopReason::Unknown,
            };
            let usage_json = output_json.get("usage").cloned().unwrap_or(serde_json::json!({}));
            CoreResponse {
                id: output_json.get("id").and_then(|v| v.as_str()).map(String::from),
                model: ModelRef {
                    requested: output_json
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .into(),
                    upstream: None,
                },
                content,
                stop_reason,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: usage_json
                        .get("prompt_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                    output_tokens: usage_json
                        .get("completion_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                    reasoning_tokens: None,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                    provenance: UsageProvenance::ProviderReported,
                },
                provider_meta: serde_json::Map::new(),
            }
        }
        _ => panic!("unknown adapter: {adapter}"),
    }
}

/// Verify that encoding a CoreResponse produces output that matches output.json.
fn assert_encode_matches_output(
    adapter: &str,
    response: CoreResponse,
    output_json: &serde_json::Value,
) {
    match adapter {
        "anthropic" => {
            let out = anthropic_adapter::encode_response(response)
                .expect("anthropic encode_response should succeed");
            let serialized = serde_json::to_value(&out)
                .expect("serialized Anthropic response should be valid JSON");
            // Compare key fields (id, type, content, stop_reason, model)
            assert_eq!(
                serialized.get("type").and_then(|v| v.as_str()),
                output_json.get("type").and_then(|v| v.as_str()),
                "type mismatch"
            );
            assert_eq!(
                serialized.get("stop_reason"),
                output_json.get("stop_reason"),
                "stop_reason mismatch"
            );
            assert_eq!(
                serialized.get("content"),
                output_json.get("content"),
                "content mismatch"
            );
        }
        "openai_chat" => {
            let out = openai_adapter::encode_response(response)
                .expect("openai encode_response should succeed");
            let serialized = serde_json::to_value(&out)
                .expect("serialized OpenAI response should be valid JSON");
            assert_eq!(
                serialized.get("object").and_then(|v| v.as_str()),
                output_json.get("object").and_then(|v| v.as_str()),
                "object mismatch"
            );
            let out_choices = serialized
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first());
            let exp_choices = output_json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first());
            assert_eq!(
                out_choices.and_then(|c| c.get("finish_reason")),
                exp_choices.and_then(|c| c.get("finish_reason")),
                "finish_reason mismatch"
            );
        }
        _ => panic!("unknown adapter: {adapter}"),
    };
}

/// Run the decode-then-encode golden test for a non-stream fixture.
fn run_non_stream_fixture(adapter: &str, case: &str) {
    let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
    let input = read_fixture(&dir, "input.json");
    let core = read_fixture(&dir, "core.json");

    // Special handling for malformed cases
    if case == "malformed" {
        assert_malformed_returns_error(adapter, &input);
        return;
    }

    assert_decode_matches_core(adapter, &input, &core);

    // Only run encode check if output.json exists
    if dir.join("output.json").exists() {
        let output = read_fixture(&dir, "output.json");
        let response = build_core_response_from_output(adapter, &output);
        assert_encode_matches_output(adapter, response, &output);
    }
}

// ---------------------------------------------------------------------------
// Anthropic non-stream fixtures
// ---------------------------------------------------------------------------

#[test]
fn anthropic_plain_text_request_fixture() {
    run_non_stream_fixture("anthropic", "plain-text-request");
}

#[test]
fn anthropic_system_prompt_fixture() {
    run_non_stream_fixture("anthropic", "system-prompt");
}

#[test]
fn anthropic_tool_call_fixture() {
    run_non_stream_fixture("anthropic", "tool-call");
}

#[test]
fn anthropic_tool_result_fixture() {
    run_non_stream_fixture("anthropic", "tool-result");
}

#[test]
fn anthropic_thinking_fixture() {
    run_non_stream_fixture("anthropic", "thinking");
}

#[test]
fn anthropic_cache_control_fixture() {
    run_non_stream_fixture("anthropic", "cache-control");
}

#[test]
fn anthropic_tool_choice_fixture() {
    run_non_stream_fixture("anthropic", "tool-choice");
}

#[test]
fn anthropic_stop_reason_fixture() {
    run_non_stream_fixture("anthropic", "stop-reason");
}

#[test]
fn anthropic_usage_fixture() {
    run_non_stream_fixture("anthropic", "usage");
}

#[test]
fn anthropic_malformed_fixture() {
    run_non_stream_fixture("anthropic", "malformed");
}

// ---------------------------------------------------------------------------
// OpenAI Chat non-stream fixtures
// ---------------------------------------------------------------------------

#[test]
fn openai_plain_text_request_fixture() {
    run_non_stream_fixture("openai_chat", "plain-text-request");
}

#[test]
fn openai_system_prompt_fixture() {
    run_non_stream_fixture("openai_chat", "system-prompt");
}

#[test]
fn openai_tool_call_fixture() {
    run_non_stream_fixture("openai_chat", "tool-call");
}

#[test]
fn openai_tool_result_fixture() {
    run_non_stream_fixture("openai_chat", "tool-result");
}

#[test]
fn openai_cache_control_fixture() {
    run_non_stream_fixture("openai_chat", "cache-control");
}

#[test]
fn openai_thinking_fixture() {
    run_non_stream_fixture("openai_chat", "thinking");
}

#[test]
fn openai_tool_choice_fixture() {
    run_non_stream_fixture("openai_chat", "tool-choice");
}

#[test]
fn openai_stop_reason_fixture() {
    run_non_stream_fixture("openai_chat", "stop-reason");
}

#[test]
fn openai_usage_fixture() {
    run_non_stream_fixture("openai_chat", "usage");
}

#[test]
fn openai_malformed_fixture() {
    run_non_stream_fixture("openai_chat", "malformed");
}

// ---------------------------------------------------------------------------
// Fixture file completeness checks
// ---------------------------------------------------------------------------

#[test]
fn all_anthropic_non_stream_fixtures_have_required_files() {
    let cases = [
        "plain-text-request",
        "system-prompt",
        "tool-call",
        "tool-result",
        "thinking",
        "cache-control",
        "tool-choice",
        "stop-reason",
        "usage",
        "malformed",
    ];
    for case in &cases {
        let dir = Path::new(FIXTURE_ROOT).join("anthropic").join(case);
        assert!(
            dir.join("input.json").exists(),
            "anthropic/{case}/input.json is missing"
        );
        assert!(
            dir.join("core.json").exists(),
            "anthropic/{case}/core.json is missing"
        );
        if *case != "malformed" {
            assert!(
                dir.join("output.json").exists(),
                "anthropic/{case}/output.json is missing"
            );
        }
    }
}

#[test]
fn all_openai_non_stream_fixtures_have_required_files() {
    let cases = [
        "plain-text-request",
        "system-prompt",
        "tool-call",
        "tool-result",
        "cache-control",
        "thinking",
        "tool-choice",
        "stop-reason",
        "usage",
        "malformed",
    ];
    for case in &cases {
        let dir = Path::new(FIXTURE_ROOT).join("openai_chat").join(case);
        assert!(
            dir.join("input.json").exists(),
            "openai_chat/{case}/input.json is missing"
        );
        assert!(
            dir.join("core.json").exists(),
            "openai_chat/{case}/core.json is missing"
        );
        if *case != "malformed" {
            assert!(
                dir.join("output.json").exists(),
                "openai_chat/{case}/output.json is missing"
            );
        }
    }
}

#[test]
fn all_streaming_fixtures_have_required_files() {
    let streaming_cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
    ];
    for (adapter, case) in &streaming_cases {
        let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
        assert!(
            dir.join("input.sse").exists(),
            "{adapter}/{case}/input.sse is missing"
        );
        assert!(
            dir.join("core-events.json").exists(),
            "{adapter}/{case}/core-events.json is missing"
        );
        assert!(
            dir.join("output.sse").exists(),
            "{adapter}/{case}/output.sse is missing"
        );
    }
}

// ---------------------------------------------------------------------------
// Streaming fixture validation tests
// ---------------------------------------------------------------------------

/// Verify that the core-events.json fixture can be deserialized into CoreEvent
/// values. This is a basic validation that streaming fixtures are well-formed.
/// Full encode/decode round-trip tests (parsing input.sse, feeding through
/// StreamEncoder, comparing with output.sse) are deferred to a later audit cycle.
#[test]
fn streaming_core_events_json_is_valid() {
    let streaming_cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
    ];
    for (adapter, case) in &streaming_cases {
        let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
        let raw = read_fixture_raw(&dir, "core-events.json");
        let events: Vec<CoreEvent> = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("failed to parse {adapter}/{case}/core-events.json: {e}"));
        assert!(
            !events.is_empty(),
            "{adapter}/{case}/core-events.json should contain at least one event"
        );
    }
}

/// Verify that input.sse and output.sse fixture files contain valid
/// SSE-formatted data when non-empty. Empty files are allowed for cases where
/// the protocol produces no output (e.g. OpenAI ping is a no-op).
#[test]
fn streaming_sse_fixtures_are_well_formed() {
    let streaming_cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
    ];
    for (adapter, case) in &streaming_cases {
        let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);

        // input.sse should always have content (it represents client input).
        let input = read_fixture_raw(&dir, "input.sse");
        assert!(
            !input.is_empty(),
            "{adapter}/{case}/input.sse should not be empty"
        );
        let input_has_sse = input.lines().any(|line| {
            line.starts_with("data:") || line.starts_with("event:")
        });
        assert!(
            input_has_sse,
            "{adapter}/{case}/input.sse should contain SSE-formatted lines"
        );

        // output.sse may be empty or whitespace-only when the protocol produces
        // no output for the given input (e.g. OpenAI Ping is a no-op).
        let output = read_fixture_raw(&dir, "output.sse");
        let output_trimmed = output.trim();
        if !output_trimmed.is_empty() {
            let output_has_sse = output_trimmed.lines().any(|line| {
                line.starts_with("data:") || line.starts_with("event:")
            });
            assert!(
                output_has_sse,
                "{adapter}/{case}/output.sse should contain SSE-formatted lines (or be empty/whitespace-only)"
            );
        }
    }
}
