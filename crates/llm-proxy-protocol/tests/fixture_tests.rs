//! Fixture-driven integration tests for client protocol adapters.
//!
//! Loads each fixture's input.json, decodes it to core, compares with core.json,
//! then encodes a response from core and compares with output.json.
//!
//! # Fixture format: client vs provider
//!
//! **Client-side fixtures** (this file) use serde's externally-tagged enum format
//! in `core-events.json` (e.g. `{"TextDelta": {"index": 0, "text": "Hello"}}`).
//! This matches how `CoreEvent` serializes with default serde derive.
//!
//! **Provider-side fixtures** (in `llm-proxy-provider/tests/fixture_tests.rs`) use
//! a human-readable format with a `"type"` field per event (e.g.
//! `{"type": "text_delta", "text": "Hello"}`). This format is compared against the
//! actual adapter output via `event_variant_name()`.
//!
//! The two formats serve different purposes: client fixtures test serde round-trips,
//! while provider fixtures test adapter decode output against human-readable expected
//! values. This is a deliberate design choice.

use llm_proxy_protocol::anthropic::{MessageEvent, MessageRequest as AnthropicMessageRequest};
use llm_proxy_protocol::client::{anthropic as anthropic_adapter, openai_chat as openai_adapter};
use llm_proxy_protocol::core::{
    CoreContent, CoreEvent, CoreResponse, ModelRef, StopReason, Usage, UsageProvenance,
};
use llm_proxy_protocol::openai::{ChatCompletionChunk, ChatCompletionRequest};

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
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
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
    let serialized = serde_json::to_value(&core_request).expect("core request should serialize");
    assert_eq!(
        serialized, *core_json,
        "decoded core does not match core.json for adapter {adapter}"
    );
}

/// Verify that a malformed input produces a ProtocolError.
///
/// Both serde deserialization failures AND semantic validation errors
/// (e.g. empty model, empty messages) are treated as expected errors.
fn assert_malformed_returns_error(adapter: &str, input_json: &serde_json::Value) {
    let result = match adapter {
        "anthropic" => {
            let req: AnthropicMessageRequest = match serde_json::from_value(input_json.clone()) {
                Ok(r) => r,
                Err(_) => return, // serde parse failure is also a valid error
            };
            anthropic_adapter::decode_request(req)
        }
        "openai_chat" => {
            let req: ChatCompletionRequest = match serde_json::from_value(input_json.clone()) {
                Ok(r) => r,
                Err(_) => return, // serde parse failure is also a valid error
            };
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
                                    text: block
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                    cache: None,
                                }),
                                "tool_use" => Some(CoreContent::ToolUse {
                                    id: block
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                    name: block
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                    input: block.get("input").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "thinking" => Some(CoreContent::Thinking {
                                    text: block
                                        .get("thinking")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                    signature: block
                                        .get("signature")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                }),
                                "redacted_thinking" => Some(CoreContent::RedactedThinking {
                                    data: block.get("data").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "image" => Some(CoreContent::Image {
                                    source: block.get("source").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "document" => Some(CoreContent::Document {
                                    source: block.get("source").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "audio" => Some(CoreContent::Audio {
                                    source: block.get("source").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "video" => Some(CoreContent::Video {
                                    source: block.get("source").cloned().unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    ),
                                }),
                                "refusal" => Some(CoreContent::Refusal {
                                    text: block
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                }),
                                "tool_result" => Some(CoreContent::ToolResult {
                                    tool_use_id: block
                                        .get("tool_use_id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .into(),
                                    content: Vec::new(),
                                    is_error: block
                                        .get("is_error")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                }),
                                _ => panic!(
                                    "build_core_response_from_output (anthropic): \
                                     unknown content type in output.json. Add a branch."
                                ),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let stop_reason = match output_json
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("anthropic output.json must contain 'stop_reason' field"))
            {
                "end_turn" => StopReason::EndTurn,
                "max_tokens" => StopReason::MaxTokens,
                "tool_use" => StopReason::ToolUse,
                "stop_sequence" => StopReason::StopSequence,
                _ => StopReason::Unknown,
            };
            let usage_json = output_json
                .get("usage")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            CoreResponse {
                id: output_json
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
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
                    content.insert(
                        0,
                        CoreContent::Thinking {
                            text: reasoning.into(),
                            signature: None,
                        },
                    );
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
                // Handle refusal content in OpenAI responses.
                if let Some(refusal) = msg.get("refusal").and_then(|v| v.as_str()) {
                    if !refusal.is_empty() {
                        content.push(CoreContent::Refusal {
                            text: refusal.into(),
                        });
                    }
                }
            }
            let finish_reason = choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("openai_chat output.json must contain 'finish_reason' in choices[0]")
                });
            let stop_reason = match finish_reason {
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "tool_calls" => StopReason::ToolUse,
                "content_filter" => StopReason::Refusal,
                _ => StopReason::Unknown,
            };
            let usage_json = output_json
                .get("usage")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            CoreResponse {
                id: output_json
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
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
            // Compare message content
            let out_msg = out_choices.and_then(|c| c.get("message"));
            let exp_msg = exp_choices.and_then(|c| c.get("message"));
            if let (Some(out_m), Some(exp_m)) = (out_msg, exp_msg) {
                assert_eq!(
                    out_m.get("content"),
                    exp_m.get("content"),
                    "message content mismatch"
                );
                // Compare tool_calls if present
                if exp_m.get("tool_calls").is_some() {
                    let out_tcs = out_m.get("tool_calls").and_then(|v| v.as_array());
                    let exp_tcs = exp_m.get("tool_calls").and_then(|v| v.as_array());
                    assert_eq!(
                        out_tcs.map(|a| a.len()),
                        exp_tcs.map(|a| a.len()),
                        "tool_calls count mismatch"
                    );
                    if let (Some(out_tc_arr), Some(exp_tc_arr)) = (out_tcs, exp_tcs) {
                        for (i, (otc, etc)) in out_tc_arr.iter().zip(exp_tc_arr.iter()).enumerate()
                        {
                            if etc.get("id").is_some() {
                                assert_eq!(
                                    otc.get("id"),
                                    etc.get("id"),
                                    "tool_calls[{i}].id mismatch"
                                );
                            }
                            assert_eq!(
                                otc.get("function").and_then(|f| f.get("name")),
                                etc.get("function").and_then(|f| f.get("name")),
                                "tool_calls[{i}].function.name mismatch"
                            );
                        }
                    }
                }
            }
            // Compare model if present in expected output
            if output_json.get("model").is_some() {
                assert_eq!(
                    serialized.get("model"),
                    output_json.get("model"),
                    "model mismatch"
                );
            }
            // Compare usage if present in expected output
            if let Some(exp_usage) = output_json.get("usage") {
                let out_usage = serialized.get("usage");
                assert!(
                    out_usage.is_some(),
                    "expected usage in encoded output but got none"
                );
                if let Some(ou) = out_usage {
                    assert_eq!(
                        ou.get("prompt_tokens"),
                        exp_usage.get("prompt_tokens"),
                        "usage.prompt_tokens mismatch"
                    );
                    assert_eq!(
                        ou.get("completion_tokens"),
                        exp_usage.get("completion_tokens"),
                        "usage.completion_tokens mismatch"
                    );
                }
            }
        }
        _ => panic!("unknown adapter: {adapter}"),
    };
}

/// Run the decode-then-encode golden test for a non-stream fixture.
fn run_non_stream_fixture(adapter: &str, case: &str) {
    let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
    let input = read_fixture(&dir, "input.json");

    // Special handling for malformed cases -- only input.json is needed.
    if case == "malformed" {
        assert_malformed_returns_error(adapter, &input);
        return;
    }

    let core = read_fixture(&dir, "core.json");

    assert_decode_matches_core(adapter, &input, &core);

    // Encode direction: construct a CoreResponse from the adapter-specific
    // output.json via build_core_response_from_output, encode it back through
    // the adapter, and compare key fields with output.json.
    //
    // NOTE: This is not a fully independent round-trip -- the CoreResponse is
    // built from the same output.json it is compared against.  The decode
    // direction (above) is the true cross-format test.  The encode direction
    // primarily verifies that encode_response produces structurally correct
    // output for a given CoreResponse.  If core-response.json fixtures are
    // added in a future audit, the encode test should construct CoreResponse
    // from those instead.
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
        if *case != "malformed" {
            assert!(
                dir.join("core.json").exists(),
                "anthropic/{case}/core.json is missing"
            );
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
        if *case != "malformed" {
            assert!(
                dir.join("core.json").exists(),
                "openai_chat/{case}/core.json is missing"
            );
            assert!(
                dir.join("output.json").exists(),
                "openai_chat/{case}/output.json is missing"
            );
        }
    }
}

/// Verify that all streaming fixtures have the required files.
///
/// Client streaming fixtures require:
/// - `input.sse`: Optional reference file showing the corresponding provider wire
///   format. Not consumed by any test, but serves as documentation of the provider
///   stream that would produce these CoreEvents.
/// - `core-events.json`: The CoreEvent sequence to encode through the StreamEncoder.
/// - `output.sse`: The expected SSE output from the StreamEncoder.
#[test]
fn all_streaming_fixtures_have_required_files() {
    let streaming_cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("anthropic", "streaming-thinking"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
        ("openai_chat", "streaming-thinking"),
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
// Unified coverage-matrix test
// ---------------------------------------------------------------------------

/// Non-stream fixture cases required for every client adapter.
/// Each entry is (case_name, required_files).
///
/// Universal cases apply to all client adapters.  Additional cases from the
/// plan (multiple-messages, refusal, redacted-thinking, multimedia content,
/// sampling-intent, model-mapping, stop-sequence) are listed as TODO items
/// for the next audit round.
fn required_client_non_stream_cases() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "plain-text-request",
            vec!["input.json", "core.json", "output.json"],
        ),
        (
            "system-prompt",
            vec!["input.json", "core.json", "output.json"],
        ),
        ("tool-call", vec!["input.json", "core.json", "output.json"]),
        (
            "tool-result",
            vec!["input.json", "core.json", "output.json"],
        ),
        ("thinking", vec!["input.json", "core.json", "output.json"]),
        (
            "cache-control",
            vec!["input.json", "core.json", "output.json"],
        ),
        (
            "tool-choice",
            vec!["input.json", "core.json", "output.json"],
        ),
        (
            "stop-reason",
            vec!["input.json", "core.json", "output.json"],
        ),
        ("usage", vec!["input.json", "core.json", "output.json"]),
        ("malformed", vec!["input.json"]),
        // TODO: Additional cases for the next audit round:
        //   multiple-messages, refusal, redacted-thinking, image/document/audio/video
        //   content, sampling-intent, model-mapping, stop-sequence.
    ]
}

/// Streaming fixture cases required for every client adapter.
fn required_client_streaming_cases() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "streaming-text",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
        (
            "streaming-tool",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
        (
            "streaming-usage",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
        (
            "streaming-error",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
        (
            "streaming-ping",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
        (
            "streaming-thinking",
            vec!["input.sse", "core-events.json", "output.sse"],
        ),
    ]
}

/// Unified coverage-matrix test that enforces required fixture cases for
/// all client adapters.  Mirrors the provider-side coverage-matrix pattern.
/// `cargo test` must fail when a required fixture is missing.
#[test]
fn coverage_matrix_all_required_client_fixtures_exist() {
    let adapters = ["anthropic", "openai_chat"];
    let mut missing: Vec<String> = Vec::new();

    for adapter in &adapters {
        // Non-stream cases
        for (case, files) in required_client_non_stream_cases() {
            let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
            for file in &files {
                if !dir.join(file).exists() {
                    missing.push(format!("{adapter}/{case}/{file}"));
                }
            }
        }

        // Streaming cases
        for (case, files) in required_client_streaming_cases() {
            let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);
            for file in &files {
                if !dir.join(file).exists() {
                    missing.push(format!("{adapter}/{case}/{file}"));
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "missing required client fixture files:\n  {}",
        missing.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Streaming fixture validation tests
// ---------------------------------------------------------------------------

/// Verify that the core-events.json fixture can be deserialized into CoreEvent
/// values. This is a basic validation that streaming fixtures are well-formed.
/// Full encode/decode round-trip tests (parsing input.sse through a
/// StreamDecoder, encoding CoreEvents through a StreamEncoder, comparing with
/// output.sse) are deferred to a later audit cycle.  The current tests
/// validate fixture structure only, not adapter behavior.
#[test]
fn streaming_core_events_json_is_valid() {
    let streaming_cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("anthropic", "streaming-thinking"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
        ("openai_chat", "streaming-thinking"),
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
        ("anthropic", "streaming-thinking"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
        ("openai_chat", "streaming-thinking"),
    ];
    for (adapter, case) in &streaming_cases {
        let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);

        // input.sse should always have content (it represents client input).
        let input = read_fixture_raw(&dir, "input.sse");
        assert!(
            !input.is_empty(),
            "{adapter}/{case}/input.sse should not be empty"
        );
        let input_has_sse = input
            .lines()
            .any(|line| line.starts_with("data:") || line.starts_with("event:"));
        assert!(
            input_has_sse,
            "{adapter}/{case}/input.sse should contain SSE-formatted lines"
        );

        // output.sse may be empty or whitespace-only when the protocol produces
        // no output for the given input (e.g. OpenAI Ping is a no-op).
        let output = read_fixture_raw(&dir, "output.sse");
        let output_trimmed = output.trim();
        if !output_trimmed.is_empty() {
            let output_has_sse = output_trimmed
                .lines()
                .any(|line| line.starts_with("data:") || line.starts_with("event:"));
            assert!(
                output_has_sse,
                "{adapter}/{case}/output.sse should contain SSE-formatted lines (or be empty/whitespace-only)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming encode round-trip tests
// ---------------------------------------------------------------------------

/// Streaming round-trip test: parse core-events.json into CoreEvent values,
/// encode them through the client adapter's StreamEncoder, format the output
/// as SSE, and verify that encoding succeeds and produces valid SSE output.
///
/// This test validates that the encode path produces structurally valid SSE
/// output for the given CoreEvent sequence. It does NOT compare exact byte
/// output with output.sse because the encoder may emit additional framing
/// events (e.g. content_block_start/stop) that are not explicit in the
/// minimal core-events.json representation.
///
/// The test covers error and ping events in addition to text/tool/usage/thinking
/// events to ensure the encode path handles all CoreEvent variants.
#[test]
fn streaming_encode_round_trip() {
    // Cases where output.sse is non-empty.
    let cases = [
        ("anthropic", "streaming-text"),
        ("anthropic", "streaming-tool"),
        ("anthropic", "streaming-usage"),
        ("anthropic", "streaming-thinking"),
        ("anthropic", "streaming-error"),
        ("anthropic", "streaming-ping"),
        ("openai_chat", "streaming-text"),
        ("openai_chat", "streaming-tool"),
        ("openai_chat", "streaming-usage"),
        ("openai_chat", "streaming-thinking"),
        ("openai_chat", "streaming-error"),
        ("openai_chat", "streaming-ping"),
    ];

    for (adapter, case) in &cases {
        let dir = Path::new(FIXTURE_ROOT).join(adapter).join(case);

        let core_events_raw = read_fixture_raw(&dir, "core-events.json");
        let events: Vec<CoreEvent> = serde_json::from_str(&core_events_raw)
            .unwrap_or_else(|e| panic!("failed to parse {adapter}/{case}/core-events.json: {e}"));

        // Extract msg_id and model from MessageStart event (or use defaults).
        let default_id = if *adapter == "anthropic" {
            "msg_default".to_owned()
        } else {
            "chatcmpl-default".to_owned()
        };
        let msg_id = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::MessageStart { id, .. } => id.clone(),
                _ => None,
            })
            .unwrap_or(default_id);
        let model = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::MessageStart { model, .. } => Some(model.requested.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_owned());

        match *adapter {
            "anthropic" => {
                let mut encoder = anthropic_adapter::StreamEncoder::new(msg_id, model);
                let mut total_data_lines = 0;

                for event in &events {
                    let message_events = encoder.encode_event(event.clone()).unwrap_or_else(|e| {
                        panic!("encode_event failed for {adapter}/{case}: {e}")
                    });
                    for me in &message_events {
                        verify_anthropic_event_json(adapter, case, me);
                        total_data_lines += 1;
                    }
                }

                let final_events = encoder
                    .finish()
                    .unwrap_or_else(|e| panic!("finish failed for {adapter}/{case}: {e}"));
                for me in &final_events {
                    verify_anthropic_event_json(adapter, case, me);
                    total_data_lines += 1;
                }

                assert!(
                    total_data_lines > 0,
                    "{adapter}/{case}: encoder should produce at least one SSE event"
                );
            }
            "openai_chat" => {
                let mut encoder = openai_adapter::StreamEncoder::new(msg_id, model, 1000, false);
                let mut total_data_lines = 0;

                for event in &events {
                    // OpenAI encoder returns Err for CoreEvent::Error by design --
                    // errors are handled at the transport level (HTTP status codes),
                    // not in stream encoding. We accept the Err gracefully and
                    // verify that non-error events still encode correctly.
                    match encoder.encode_event(event.clone()) {
                        Ok(chunks) => {
                            for chunk in &chunks {
                                verify_openai_chunk_json(adapter, case, chunk);
                                total_data_lines += 1;
                            }
                        }
                        Err(_) => {
                            // Expected for CoreEvent::Error on OpenAI Chat.
                            // Verify this was indeed an error event.
                            assert!(
                                matches!(event, CoreEvent::Error { .. }),
                                "{adapter}/{case}: encode_event failed for non-error event"
                            );
                        }
                    }
                }

                let final_chunks = encoder
                    .finish()
                    .unwrap_or_else(|e| panic!("finish failed for {adapter}/{case}: {e}"));
                for chunk in &final_chunks {
                    verify_openai_chunk_json(adapter, case, chunk);
                    total_data_lines += 1;
                }

                // Verify at least some chunks were produced, unless the fixture
                // is an error-only case where OpenAI intentionally returns Err.
                let is_error_only = events
                    .iter()
                    .all(|e| matches!(e, CoreEvent::Error { .. } | CoreEvent::Ping));
                if !is_error_only {
                    assert!(
                        total_data_lines > 0,
                        "{adapter}/{case}: encoder should produce at least one SSE chunk"
                    );
                }
            }
            _ => panic!("unknown adapter: {adapter}"),
        }
    }
}

/// Verify an Anthropic MessageEvent is valid JSON with a known event type.
fn verify_anthropic_event_json(adapter: &str, case: &str, me: &MessageEvent) {
    let json = serde_json::to_string(me)
        .unwrap_or_else(|e| panic!("failed to serialize MessageEvent for {adapter}/{case}: {e}"));
    let known_types = [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "content_block_stop",
        "message_delta",
        "message_stop",
        "ping",
        "error",
    ];
    assert!(
        known_types.contains(&me.r#type.as_str()),
        "{adapter}/{case}: unknown event type '{}'",
        me.r#type
    );
    assert!(
        json.starts_with('{') && json.ends_with('}'),
        "{adapter}/{case}: MessageEvent JSON should be an object, got: {json}"
    );
}

/// Verify an OpenAI ChatCompletionChunk is valid JSON.
fn verify_openai_chunk_json(adapter: &str, case: &str, chunk: &ChatCompletionChunk) {
    let json = serde_json::to_string(chunk).unwrap_or_else(|e| {
        panic!("failed to serialize ChatCompletionChunk for {adapter}/{case}: {e}")
    });
    assert!(
        json.starts_with('{') && json.ends_with('}'),
        "{adapter}/{case}: ChatCompletionChunk JSON should be an object, got: {json}"
    );
}
