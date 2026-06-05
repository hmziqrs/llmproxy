//! Request transformation from Anthropic Messages API to OpenAI, Responses, and Gemini formats.
//!
//! Ported from `oc-go-cc/internal/transformer/request.go`.  The public API
//! consists of four free functions that convert an [`MessageRequest`] into the
//! appropriate upstream format, driven by a [`ModelConfig`].

use llm_proxy_core::config::ModelConfig;

use crate::anthropic::{ContentBlock, Message, MessageRequest};
use crate::openai::{
    CacheControl as OpenAICacheControl, ChatCompletionRequest, ChatMessage, FunctionCall,
    FunctionDef, StreamOptions, ToolCall, ToolDef,
};
use crate::zen::{
    GeminiContent, GeminiFunctionDeclaration, GeminiGenerationConfig, GeminiPart, GeminiRequest,
    GeminiTool, ResponsesInput, ResponsesReasoning, ResponsesRequest, ResponsesTool,
};

// ---------------------------------------------------------------------------
// Model-family helpers
// ---------------------------------------------------------------------------

/// Returns `true` for DeepSeek models that require thinking-mode handling.
fn is_deepseek_model(model_id: &str) -> bool {
    model_id.starts_with("deepseek-")
}

/// Returns `true` for OpenAI o1 / o3 reasoning models.
fn is_openai_reasoning_model(model_id: &str) -> bool {
    model_id.starts_with("o1-") || model_id.starts_with("o3-")
}

/// Returns `true` for providers whose validators require a non-empty
/// `reasoning_content` field on assistant tool-call messages (e.g. Moonshot).
fn needs_placeholder_reasoning(model_id: &str) -> bool {
    model_id.starts_with("kimi-")
}

// ---------------------------------------------------------------------------
// Thinking helpers
// ---------------------------------------------------------------------------

/// Checks if the `thinking` JSON value explicitly sets `type` to `"disabled"`.
fn is_thinking_disabled(thinking: &serde_json::Value) -> bool {
    thinking
        .as_object()
        .and_then(|m| m.get("type"))
        .and_then(|v| v.as_str())
        == Some("disabled")
}

/// Returns `true` when any assistant message in `messages` contains thinking
/// content -- either as a dedicated `thinking`-typed block, or attached as a
/// non-empty `thinking` field on a `tool_use` block.
#[must_use]
pub fn has_thinking_blocks(messages: &[Message]) -> bool {
    for msg in messages {
        if msg.role != "assistant" {
            continue;
        }
        for block in msg.content_blocks() {
            if block.r#type == "thinking" {
                return true;
            }
            if block.r#type == "tool_use" && block.thinking.as_ref().is_some_and(|t| !t.is_empty())
            {
                return true;
            }
        }
    }
    false
}

/// Returns `true` when the conversation contains at least one assistant message.
fn has_assistant_messages(messages: &[Message]) -> bool {
    messages.iter().any(|m| m.role == "assistant")
}

/// Maps Anthropic `budget_tokens` to an OpenAI `reasoning_effort` string.
fn budget_tokens_to_effort(budget: i64) -> &'static str {
    if budget <= 2048 {
        "low"
    } else if budget <= 8192 {
        "medium"
    } else if budget <= 32768 {
        "high"
    } else {
        "max"
    }
}

/// Extracts `budget_tokens` from a thinking JSON value.
fn parse_budget_tokens(thinking: &serde_json::Value) -> i64 {
    thinking
        .as_object()
        .and_then(|m| m.get("budget_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Sets `reasoning_effort` on the request, defaulting to `"high"` when the
/// config value is empty.
fn set_reasoning_effort(openai_req: &mut ChatCompletionRequest, effort: &str) {
    let val = if effort.is_empty() {
        "high".to_owned()
    } else {
        effort.to_owned()
    };
    openai_req.reasoning_effort = Some(val);
}

/// Applies thinking / reasoning_effort to the OpenAI request.
///
/// Decision priority:
///
/// 1. **Client request** -- `anthropic_req.thinking` set and not disabled.
/// 2. **History continuity** -- a prior turn used thinking.
/// 3. **Explicit config** -- `model.thinking` set.
/// 4. **Config intent** -- `model.reasoning_effort` set without `model.thinking`.
/// 5. **No config, no history** -- leave both unset.
fn resolve_thinking_and_effort(
    anthropic_req: &MessageRequest,
    model: &ModelConfig,
    openai_req: &mut ChatCompletionRequest,
) {
    let has_thinking = has_thinking_blocks(&anthropic_req.messages);
    let has_assistant = has_assistant_messages(&anthropic_req.messages);
    let explicit_thinking = model.thinking.is_some();
    let explicit_effort = !model.reasoning_effort.is_empty();
    let is_deepseek = is_deepseek_model(&model.model_id);
    let is_openai_reasoning = is_openai_reasoning_model(&model.model_id);

    let request_thinking_disabled = anthropic_req
        .thinking
        .as_ref()
        .is_some_and(is_thinking_disabled);
    let request_thinking = !request_thinking_disabled
        && anthropic_req
            .thinking
            .as_ref()
            .is_some_and(|t| !t.is_null());

    let allow_thinking_param = is_deepseek || explicit_thinking;
    let allow_effort_param = is_openai_reasoning || is_deepseek || explicit_effort;

    // Case: client explicitly disabled thinking.
    if request_thinking_disabled {
        if allow_thinking_param {
            openai_req.thinking = anthropic_req.thinking.clone();
        }
        return;
    }

    // Safety guard: DeepSeek + has assistant messages but no thinking blocks
    // in history -> disable thinking to avoid 400s from upstream.
    if is_deepseek && has_assistant && !has_thinking {
        if allow_thinking_param {
            openai_req.thinking = Some(serde_json::json!({"type": "disabled"}));
        }
        return;
    }

    match (
        request_thinking,
        has_thinking,
        explicit_thinking,
        explicit_effort,
    ) {
        // 1. Client explicitly opted into thinking mode.
        (true, _, _, _) => {
            if allow_thinking_param {
                openai_req.thinking = anthropic_req.thinking.clone();
            }
            if allow_effort_param {
                if let Some(ref thinking) = anthropic_req.thinking {
                    let budget = parse_budget_tokens(thinking);
                    if budget > 0 {
                        let effort = budget_tokens_to_effort(budget).to_owned();
                        openai_req.reasoning_effort = Some(effort);
                    }
                }
            }
        }

        // 2. History has thinking blocks -- maintain continuity.
        (false, true, _, _) => {
            if allow_thinking_param {
                if explicit_thinking {
                    openai_req.thinking = model.thinking.clone();
                } else {
                    openai_req.thinking = Some(serde_json::json!({"type": "enabled"}));
                }
            }
            if allow_effort_param {
                let thinking_disabled = openai_req
                    .thinking
                    .as_ref()
                    .is_some_and(is_thinking_disabled);
                if !thinking_disabled || !is_deepseek {
                    set_reasoning_effort(openai_req, &model.reasoning_effort);
                }
            }
        }

        // 3. Config explicitly sets thinking -- respect it.
        (false, false, true, _) => {
            if allow_thinking_param {
                openai_req.thinking = model.thinking.clone();
            }
            if allow_effort_param {
                let thinking_disabled = openai_req
                    .thinking
                    .as_ref()
                    .is_some_and(is_thinking_disabled);
                if !thinking_disabled || !is_deepseek {
                    set_reasoning_effort(openai_req, &model.reasoning_effort);
                }
            }
        }

        // 4. User set reasoning_effort but not thinking. Intent is clear.
        (false, false, false, true) => {
            if allow_thinking_param {
                openai_req.thinking = Some(serde_json::json!({"type": "enabled"}));
            }
            if allow_effort_param {
                set_reasoning_effort(openai_req, &model.reasoning_effort);
            }
        }

        // 5. No config, no history: leave both unset.
        (false, false, false, false) => {
            // No thinking configuration and no history of thinking blocks.
            // Leave both `thinking` and `reasoning_effort` unset.
        }
    }
}

// ---------------------------------------------------------------------------
// Cache-control helper
// ---------------------------------------------------------------------------

/// Strips `cache_control` from all messages in the list (in place).
fn strip_cache_control(messages: &mut [ChatMessage]) {
    for msg in messages.iter_mut() {
        msg.cache_control = None;
    }
}

// ---------------------------------------------------------------------------
// Message transformation
// ---------------------------------------------------------------------------

/// Converts Anthropic messages to OpenAI format.
fn transform_messages(
    anthropic_req: &MessageRequest,
    model_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    let has_thinking = has_thinking_blocks(&anthropic_req.messages);

    let mut result: Vec<ChatMessage> = Vec::new();

    // Add system message if present, preserving cache_control if available.
    let system_text = anthropic_req.system_text();
    if !system_text.is_empty() {
        let mut system_msg = ChatMessage {
            role: "system".to_owned(),
            content: system_text,
            reasoning_content: None,
            tool_calls: Vec::new(),
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        };

        // Try to extract cache_control from system array blocks.
        if let Some(ref sys_val) = anthropic_req.system {
            if let Some(arr) = sys_val.as_array() {
                for item in arr {
                    if let Ok(block) =
                        serde_json::from_value::<crate::anthropic::SystemContentBlock>(item.clone())
                    {
                        if block.r#type == "text" {
                            if let Some(ref cc) = block.cache_control {
                                system_msg.cache_control = Some(OpenAICacheControl {
                                    r#type: cc.r#type.clone(),
                                });
                                break;
                            }
                        }
                    }
                }
            }
        }

        result.push(system_msg);
    }

    // Transform each message.
    for msg in &anthropic_req.messages {
        let openai_msgs = transform_message(msg, model_id, has_thinking)?;
        result.extend(openai_msgs);
    }

    Ok(result)
}

/// Converts a single Anthropic message to one or more OpenAI messages.
fn transform_message(
    msg: &Message,
    model_id: &str,
    has_thinking_in_history: bool,
) -> Result<Vec<ChatMessage>, String> {
    let blocks = msg.content_blocks();

    match msg.role.as_str() {
        "user" => transform_user_message(&blocks),
        "assistant" => transform_assistant_message(&blocks, model_id, has_thinking_in_history),
        _ => {
            // Fallback: concatenate all text.
            let text: String = blocks
                .iter()
                .filter(|b| b.r#type == "text")
                .filter_map(|b| b.text.as_deref())
                .collect();
            Ok(vec![ChatMessage {
                role: msg.role.clone(),
                content: text,
                reasoning_content: None,
                tool_calls: Vec::new(),
                name: None,
                tool_call_id: None,
                cache_control: None,
                    refusal: None,
            }])
        }
    }
}

/// Converts a user message with potential `tool_result` blocks.
fn transform_user_message(blocks: &[ContentBlock]) -> Result<Vec<ChatMessage>, String> {
    let mut result: Vec<ChatMessage> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    for block in blocks {
        match block.r#type.as_str() {
            "text" => {
                if let Some(ref t) = block.text {
                    text_parts.push(t.clone());
                }
            }
            "tool_result" => {
                let tool_content = block.text_content();
                result.push(ChatMessage {
                    role: "tool".to_owned(),
                    content: tool_content,
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    name: None,
                    tool_call_id: Some(block.get_tool_id()),
                    cache_control: None,
                    refusal: None,
                });
            }
            "image" => {
                // Images not supported in text-only models, skip.
                text_parts.push("[Image]".to_owned());
            }
            // TODO: Unrecognized content block types are silently dropped.
            // The protocol crate intentionally does not depend on `tracing`,
            // so no warning is emitted. The core protocol migration should
            // log a warning via the adapter layer. This is a known gap per
            // protocol-normalization.md Section 7: "Drop the field and
            // record a warning."
            _ => {}
        }
    }

    // If there is text content, add it as a user message after tool results.
    if !text_parts.is_empty() {
        let text = text_parts.join("");
        result.push(ChatMessage {
            role: "user".to_owned(),
            content: text,
            reasoning_content: None,
            tool_calls: Vec::new(),
            name: None,
            tool_call_id: None,
            cache_control: None,
                    refusal: None,
        });
    }

    Ok(result)
}

/// Converts an assistant message with potential `tool_use` blocks.
fn transform_assistant_message(
    blocks: &[ContentBlock],
    model_id: &str,
    has_thinking_in_history: bool,
) -> Result<Vec<ChatMessage>, String> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in blocks {
        match block.r#type.as_str() {
            "text" => {
                if let Some(ref t) = block.text {
                    text_parts.push(t.clone());
                }
            }
            "thinking" => {
                if let Some(ref t) = block.thinking {
                    if !t.is_empty() {
                        thinking_parts.push(t.clone());
                    }
                }
            }
            "tool_use" => {
                // Extract inline thinking attached to tool_use blocks.
                if let Some(ref t) = block.thinking {
                    if !t.is_empty() {
                        thinking_parts.push(t.clone());
                    }
                }

                let arguments = match &block.input {
                    Some(v) if !v.is_null() => {
                        // Legacy behavior: serialization failure silently falls back
                        // to an empty JSON object. The core protocol migration should
                        // propagate this error instead of silently substituting.
                        // TODO: Add tracing::warn! when the protocol crate gains a
                        // tracing dependency, or propagate the error via a typed
                        // TransformError enum.
                        serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned())
                    }
                    _ => "{}".to_owned(),
                };

                tool_calls.push(ToolCall {
                    index: None,
                    id: block.id.clone(),
                    r#type: Some("function".to_owned()),
                    function: Some(FunctionCall {
                        name: block.name.clone(),
                        arguments: Some(arguments),
                    }),
                });
            }
            // TODO: Unrecognized assistant content block types are silently
            // dropped. See the `_ => {}` arm in `transform_user_message` for
            // the same known gap -- the protocol crate does not depend on
            // `tracing` and cannot emit warnings.
            _ => {}
        }
    }

    // Build the assistant message.
    let content: String = text_parts.join("");
    let reasoning_text: String = thinking_parts.join("");

    let reasoning_content = if !reasoning_text.is_empty() {
        // Real thinking content from the upstream history.
        Some(reasoning_text)
    } else if has_thinking_in_history && is_deepseek_model(model_id) {
        // DeepSeek in thinking mode requires reasoning_content on EVERY
        // assistant message. Use a single-space placeholder.
        Some(" ".to_owned())
    } else if !tool_calls.is_empty() && needs_placeholder_reasoning(model_id) {
        // Moonshot's validator treats empty string as missing.
        Some(" ".to_owned())
    } else {
        None
    };

    let msg = ChatMessage {
        role: "assistant".to_owned(),
        content,
        reasoning_content,
        tool_calls,
        name: None,
        tool_call_id: None,
        cache_control: None,
                    refusal: None,
    };

    Ok(vec![msg])
}

// ---------------------------------------------------------------------------
// Tool choice mapping
// ---------------------------------------------------------------------------

/// Maps an Anthropic tool_choice value to the OpenAI equivalent.
///
/// Anthropic formats:
/// - `{"type": "auto"}` -> OpenAI `{"type": "auto"}`
/// - `{"type": "any"}` -> OpenAI `{"type": "required"}`
/// - `{"type": "tool", "name": "..."}` -> OpenAI `{"type": "function", "function": {"name": "..."}}`
/// - `{"type": "none"}` -> OpenAI `{"type": "none"}`
///
/// Unknown types are forwarded as-is (best-effort passthrough).
fn map_tool_choice(tc: &serde_json::Value) -> serde_json::Value {
    let tc_type = tc.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match tc_type {
        "auto" => serde_json::json!({"type": "auto"}),
        "any" => serde_json::json!({"type": "required"}),
        "none" => serde_json::json!({"type": "none"}),
        "tool" => {
            if let Some(name) = tc.get("name").and_then(|v| v.as_str()) {
                serde_json::json!({
                    "type": "function",
                    "function": {"name": name}
                })
            } else {
                // "tool" without a name -- fall back to "required".
                serde_json::json!({"type": "required"})
            }
        }
        _ => tc.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tool transformation
// ---------------------------------------------------------------------------

/// Converts Anthropic tools to OpenAI tool definitions.
fn transform_tools(tools: &[crate::anthropic::Tool]) -> Vec<ToolDef> {
    let mut result = Vec::with_capacity(tools.len());

    for tool in tools {
        let schema = if tool.input_schema.is_null() || tool.input_schema == serde_json::Value::Null
        {
            serde_json::json!({"type": "object", "properties": {}})
        } else {
            tool.input_schema.clone()
        };

        result.push(ToolDef {
            r#type: "function".to_owned(),
            function: FunctionDef {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: Some(schema),
            },
        });
    }

    result
}

/// Converts Anthropic tools to Responses API tool format.
fn transform_tools_for_responses(tools: &[crate::anthropic::Tool]) -> Vec<ResponsesTool> {
    let mut result = Vec::with_capacity(tools.len());

    for tool in tools {
        let schema = if tool.input_schema.is_null() || tool.input_schema == serde_json::Value::Null
        {
            serde_json::json!({"type": "object", "properties": {}})
        } else {
            tool.input_schema.clone()
        };

        result.push(ResponsesTool {
            r#type: "function".to_owned(),
            name: Some(tool.name.clone()),
            description: tool.description.clone(),
            parameters: Some(schema),
        });
    }

    result
}

/// Converts Anthropic tools to Gemini tool format.
fn transform_tools_for_gemini(tools: &[crate::anthropic::Tool]) -> Vec<GeminiTool> {
    let mut decls = Vec::with_capacity(tools.len());

    for tool in tools {
        let schema = if tool.input_schema.is_null() || tool.input_schema == serde_json::Value::Null
        {
            serde_json::json!({"type": "object", "properties": {}})
        } else {
            tool.input_schema.clone()
        };

        decls.push(GeminiFunctionDeclaration {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: Some(schema),
        });
    }

    vec![GeminiTool {
        function_declarations: decls,
    }]
}

// ===========================================================================
// Public API
// ===========================================================================

/// Converts an Anthropic [`MessageRequest`] to an OpenAI [`ChatCompletionRequest`].
///
/// This is the primary transformation function used for most upstream
/// providers (GLM, Kimi, MiMo, Qwen, DeepSeek, etc.).
///
/// Note: The Anthropic Messages API does not have a `stop` field; stop sequences
/// are not forwarded in this transformation. The `ChatCompletionRequest.stop`
/// field remains `None`.
///
/// Note: All public transformer functions return `Result<_, String>` instead of
/// a typed error enum. This is a known unidiomatic pattern -- the core protocol
/// migration should introduce a `TransformError` enum (with `thiserror`) for
/// consistency with `CoreError` and `ProviderError`.
pub fn transform_request(
    anthropic_req: &MessageRequest,
    model: &ModelConfig,
) -> Result<ChatCompletionRequest, String> {
    // Transform messages.
    let mut messages = transform_messages(anthropic_req, &model.model_id)?;

    // Strip cache_control for models that don't support it (everything except DeepSeek).
    if !is_deepseek_model(&model.model_id) {
        strip_cache_control(&mut messages);
    }

    // Build OpenAI request.
    let mut openai_req = ChatCompletionRequest {
        model: model.model_id.clone(),
        messages,
        stream: anthropic_req.stream,
        temperature: None,
        top_p: None,
        max_tokens: None,
        reasoning_effort: None,
        thinking: None,
        tools: Vec::new(),
        tool_choice: None,
        stop: None,
        stream_options: None,
        user: None,
        extra: serde_json::Map::new(),
    };

    // Add stream_options with include_usage: true for streaming.
    if anthropic_req.stream == Some(true) {
        openai_req.stream_options = Some(StreamOptions {
            include_usage: Some(true),
        });
    }

    // Copy optional parameters from Anthropic request.
    if let Some(temp) = anthropic_req.temperature {
        openai_req.temperature = Some(temp);
    }
    if let Some(top_p) = anthropic_req.top_p {
        openai_req.top_p = Some(top_p);
    }

    // Map max_tokens.
    if anthropic_req.max_tokens > 0 {
        openai_req.max_tokens = Some(anthropic_req.max_tokens);
    }

    // Apply model-specific overrides (config wins over request).
    // Legacy behavior: config-driven temperature/max_tokens overrides silently
    // discard client intent. The core protocol migration should preserve client
    // intent separately from config overrides.
    // TODO: Add a debug-level tracing event when config overrides client intent,
    // even in legacy code. Requires adding tracing dependency or deferring to
    // the adapter layer.
    if model.temperature > 0.0 {
        openai_req.temperature = Some(model.temperature);
    }
    if model.max_tokens > 0 {
        openai_req.max_tokens = Some(i32::try_from(model.max_tokens).unwrap_or(i32::MAX));
    }

    // Resolve thinking and reasoning_effort.
    resolve_thinking_and_effort(anthropic_req, model, &mut openai_req);

    // Transform tools if present.
    if !anthropic_req.tools.is_empty() {
        openai_req.tools = transform_tools(&anthropic_req.tools);
    }

    // Map tool_choice if present.
    // Anthropic uses {"type": "auto"} / {"type": "any"} / {"type": "tool", "name": "..."}
    // OpenAI uses {"type": "auto"} / {"type": "required"} / {"type": "none"} / specific function.
    // The raw JSON is forwarded, so Anthropic-specific values like "any" are mapped here.
    if let Some(ref tc) = anthropic_req.tool_choice {
        openai_req.tool_choice = Some(map_tool_choice(tc));
    }

    Ok(openai_req)
}

/// Converts an Anthropic [`MessageRequest`] to an OpenAI Responses API
/// [`ResponsesRequest`].
///
/// System messages become `developer` role inputs. Messages become inputs
/// with raw JSON content. Tool results become separate `tool` role inputs.
pub fn transform_to_responses(
    anthropic_req: &MessageRequest,
    model: &ModelConfig,
) -> Result<ResponsesRequest, String> {
    let mut input: Vec<ResponsesInput> = Vec::new();

    // Add system message if present.
    let system_text = anthropic_req.system_text();
    if !system_text.is_empty() {
        input.push(ResponsesInput {
            role: "developer".to_owned(),
            content: Some(serde_json::Value::String(system_text)),
        });
    }

    // Transform messages.
    for msg in &anthropic_req.messages {
        let blocks = msg.content_blocks();
        let mut text_parts: Vec<String> = Vec::new();

        for block in &blocks {
            match block.r#type.as_str() {
                "text" => {
                    if let Some(ref t) = block.text {
                        text_parts.push(t.clone());
                    }
                }
                "tool_result" => {
                    let tool_content = block.text_content();
                    input.push(ResponsesInput {
                        role: "tool".to_owned(),
                        content: Some(serde_json::Value::String(tool_content)),
                    });
                }
                // TODO: Unrecognized content block types (e.g. "image",
                // "thinking") in the Responses API transformer are silently
                // dropped. See the `_ => {}` arm in `transform_user_message`
                // for the same known gap.
                _ => {}
            }
        }

        if !text_parts.is_empty() {
            let text = text_parts.join("");
            input.push(ResponsesInput {
                role: msg.role.clone(),
                content: Some(serde_json::Value::String(text)),
            });
        }
    }

    let stream = anthropic_req.stream.unwrap_or(false);

    let mut req = ResponsesRequest {
        model: model.model_id.clone(),
        input,
        stream: if anthropic_req.stream.is_some() {
            Some(stream)
        } else {
            None
        },
        tools: Vec::new(),
        reasoning: None,
    };

    // Transform tools if present.
    if !anthropic_req.tools.is_empty() {
        req.tools = transform_tools_for_responses(&anthropic_req.tools);
    }

    // Add reasoning if model supports it.
    if !model.reasoning_effort.is_empty() {
        req.reasoning = Some(ResponsesReasoning {
            effort: Some(model.reasoning_effort.clone()),
        });
    }

    Ok(req)
}

/// Converts an Anthropic [`MessageRequest`] to a Google Gemini
/// [`GeminiRequest`].
///
/// System messages are prepended as user messages with `[System Instruction]`.
/// An acknowledgment model message is added. Tool results are formatted with
/// their tool ID. Assistant messages use the `"model"` role. Tools become
/// `GeminiTool` with `FunctionDeclaration`s. Generation config maps
/// `max_tokens` to `max_output_tokens`.
pub fn transform_to_gemini(
    anthropic_req: &MessageRequest,
    model: &ModelConfig,
) -> Result<GeminiRequest, String> {
    let mut contents: Vec<GeminiContent> = Vec::new();

    // Add system instruction via a user message if present.
    let system_text = anthropic_req.system_text();
    if !system_text.is_empty() {
        contents.push(GeminiContent {
            role: "user".to_owned(),
            parts: vec![GeminiPart {
                text: Some(format!("[System Instruction] {}", system_text)),
                function_call: None,
            }],
        });
        contents.push(GeminiContent {
            role: "model".to_owned(),
            parts: vec![GeminiPart {
                text: Some("Understood. I will follow these instructions.".to_owned()),
                function_call: None,
            }],
        });
    }

    // Transform messages.
    for msg in &anthropic_req.messages {
        let blocks = msg.content_blocks();
        let mut text_parts: Vec<String> = Vec::new();

        for block in &blocks {
            match block.r#type.as_str() {
                "text" => {
                    if let Some(ref t) = block.text {
                        text_parts.push(t.clone());
                    }
                }
                "tool_result" => {
                    let tool_content = block.text_content();
                    contents.push(GeminiContent {
                        role: "user".to_owned(),
                        parts: vec![GeminiPart {
                            text: Some(format!(
                                "[Tool Result for {}] {}",
                                block.get_tool_id(),
                                tool_content
                            )),
                            function_call: None,
                        }],
                    });
                }
                // TODO: Unrecognized content block types in the Gemini
                // transformer are silently dropped. See the `_ => {}` arm in
                // `transform_user_message` for the same known gap.
                _ => {}
            }
        }

        if !text_parts.is_empty() {
            let text = text_parts.join("");
            let role = if msg.role == "assistant" {
                "model"
            } else {
                &msg.role
            };
            contents.push(GeminiContent {
                role: role.to_owned(),
                parts: vec![GeminiPart { text: Some(text), function_call: None }],
            });
        }
    }

    let mut req = GeminiRequest {
        contents,
        generation_config: None,
        tools: Vec::new(),
        stream: None,
    };

    // Set generation config.
    let mut gen_config = GeminiGenerationConfig {
        temperature: None,
        max_output_tokens: None,
    };

    if anthropic_req.max_tokens > 0 {
        gen_config.max_output_tokens = Some(anthropic_req.max_tokens);
    }

    if model.temperature > 0.0 {
        gen_config.temperature = Some(model.temperature);
    } else if let Some(temp) = anthropic_req.temperature {
        gen_config.temperature = Some(temp);
    }

    if gen_config.max_output_tokens.is_some() || gen_config.temperature.is_some() {
        req.generation_config = Some(gen_config);
    }

    // Transform tools if present.
    if !anthropic_req.tools.is_empty() {
        req.tools = transform_tools_for_gemini(&anthropic_req.tools);
    }

    Ok(req)
}

/// Maps an HTTP status code and error message to an Anthropic-style error
/// JSON payload.
///
/// Status mapping:
///
/// | Code  | Error type                |
/// |-------|---------------------------|
/// | 400   | `invalid_request_error`   |
/// | 401   | `authentication_error`    |
/// | 403   | `permission_error`        |
/// | 404   | `not_found_error`         |
/// | 429   | `rate_limit_error`        |
/// | 500+  | `api_error`               |
#[must_use]
pub fn transform_error_response(status_code: u16, message: &str) -> serde_json::Value {
    let error_type = match status_code {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    };

    serde_json::json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message
        }
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{Message, MessageRequest};

    /// Helper to build a minimal `MessageRequest` for tests.
    fn make_request(messages: Vec<Message>) -> MessageRequest {
        MessageRequest {
            model: "test-model".to_owned(),
            max_tokens: 1024,
            system: None,
            messages,
            stream: None,
            tools: Vec::new(),
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
            tool_choice: None,
        }
    }

    /// Helper to build a minimal `ModelConfig` for tests.
    fn make_model(model_id: &str) -> ModelConfig {
        ModelConfig {
            provider: "test".to_owned(),
            model_id: model_id.to_owned(),
            temperature: 0.0,
            max_tokens: 0,
            context_threshold: 0,
            reasoning_effort: String::new(),
            thinking: None,
        }
    }

    fn user_msg(text: &str) -> Message {
        Message {
            role: "user".to_owned(),
            content: serde_json::Value::String(text.to_owned()),
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message {
            role: "assistant".to_owned(),
            content: serde_json::Value::String(text.to_owned()),
        }
    }

    fn assistant_msg_with_blocks(blocks: serde_json::Value) -> Message {
        Message {
            role: "assistant".to_owned(),
            content: blocks,
        }
    }

    fn user_msg_with_tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: "user".to_owned(),
            content: serde_json::json!([
                {
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content
                }
            ]),
        }
    }

    // -- transform_request ----------------------------------------------------

    #[test]
    fn transform_simple_conversation() {
        let req = make_request(vec![
            user_msg("Hello"),
            assistant_msg("Hi there"),
            user_msg("How are you?"),
        ]);
        let model = make_model("qwen3.5-plus");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.model, "qwen3.5-plus");
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content, "Hello");
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(result.messages[1].content, "Hi there");
        assert_eq!(result.messages[2].role, "user");
        assert_eq!(result.messages[2].content, "How are you?");
    }

    #[test]
    fn transform_with_system_prompt() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.system = Some(serde_json::Value::String("You are helpful".to_owned()));
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.messages[0].role, "system");
        assert_eq!(result.messages[0].content, "You are helpful");
    }

    #[test]
    fn transform_stream_adds_stream_options() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.stream = Some(true);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.stream, Some(true));
        assert!(result.stream_options.is_some());
        assert_eq!(result.stream_options.unwrap().include_usage, Some(true));
    }

    #[test]
    fn transform_no_stream_no_stream_options() {
        let req = make_request(vec![user_msg("hi")]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert!(result.stream_options.is_none());
    }

    #[test]
    fn transform_temperature_and_max_tokens() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.temperature = Some(0.5);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.temperature, Some(0.5));
        assert_eq!(result.max_tokens, Some(1024));
    }

    #[test]
    fn transform_model_overrides_temperature() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.temperature = Some(0.5);
        let mut model = make_model("glm-5.1");
        model.temperature = 0.9;
        model.max_tokens = 2048;
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.temperature, Some(0.9));
        assert_eq!(result.max_tokens, Some(2048));
    }

    #[test]
    fn transform_tool_result_becomes_tool_message() {
        let req = make_request(vec![
            assistant_msg_with_blocks(serde_json::json!([
                { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"} }
            ])),
            user_msg_with_tool_result("tu_1", "72F and sunny"),
        ]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        // First message: assistant with tool_calls.
        assert_eq!(result.messages[0].role, "assistant");
        assert_eq!(result.messages[0].tool_calls.len(), 1);
        assert_eq!(result.messages[0].tool_calls[0].id, Some("tu_1".to_owned()));
        assert_eq!(
            result.messages[0].tool_calls[0]
                .function
                .as_ref()
                .unwrap()
                .name,
            Some("get_weather".to_owned())
        );

        // Second message: tool result.
        assert_eq!(result.messages[1].role, "tool");
        assert_eq!(result.messages[1].tool_call_id, Some("tu_1".to_owned()));
        assert_eq!(result.messages[1].content, "72F and sunny");
    }

    #[test]
    fn transform_strips_cache_control_except_deepseek() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.system = Some(serde_json::json!([
            { "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }
        ]));

        // Non-DeepSeek: cache_control should be stripped.
        let model_glm = make_model("glm-5.1");
        let result_glm = transform_request(&req, &model_glm).unwrap();
        assert!(result_glm.messages[0].cache_control.is_none());

        // DeepSeek: cache_control should be preserved.
        let model_ds = make_model("deepseek-v4-pro");
        let result_ds = transform_request(&req, &model_ds).unwrap();
        assert!(result_ds.messages[0].cache_control.is_some());
    }

    #[test]
    fn transform_thinking_blocks_become_reasoning_content() {
        let req = make_request(vec![assistant_msg_with_blocks(serde_json::json!([
            { "type": "thinking", "thinking": "Let me think..." },
            { "type": "text", "text": "The answer is 42" }
        ]))]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.messages[0].role, "assistant");
        assert_eq!(result.messages[0].content, "The answer is 42");
        assert_eq!(
            result.messages[0].reasoning_content,
            Some("Let me think...".to_owned())
        );
    }

    #[test]
    fn transform_tools_converted() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.tools = vec![crate::anthropic::Tool {
            name: "get_weather".to_owned(),
            description: Some("Get the weather".to_owned()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                }
            }),
        }];
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].r#type, "function");
        assert_eq!(result.tools[0].function.name, "get_weather");
    }

    #[test]
    fn transform_deepseek_adds_placeholder_reasoning() {
        let req = make_request(vec![
            assistant_msg_with_blocks(serde_json::json!([
                { "type": "thinking", "thinking": "hmm" },
                { "type": "text", "text": "First answer" }
            ])),
            user_msg("follow up"),
            assistant_msg_with_blocks(serde_json::json!([
                { "type": "text", "text": "Second answer" }
            ])),
        ]);
        let model = make_model("deepseek-v4-pro");
        let result = transform_request(&req, &model).unwrap();

        // Third message (second assistant) should have placeholder reasoning_content.
        assert_eq!(result.messages[2].role, "assistant");
        assert_eq!(result.messages[2].reasoning_content, Some(" ".to_owned()));
    }

    // -- resolve_thinking_and_effort ------------------------------------------

    #[test]
    fn thinking_client_request_forwarded() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.thinking = Some(serde_json::json!({"type": "enabled", "budget_tokens": 4096}));
        let mut model = make_model("deepseek-v4-pro");
        model.thinking = Some(serde_json::json!({"type": "enabled"}));

        let mut openai_req = ChatCompletionRequest {
            model: "deepseek-v4-pro".to_owned(),
            messages: Vec::new(),
            stream: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            reasoning_effort: None,
            thinking: None,
            tools: Vec::new(),
            tool_choice: None,
            stop: None,
            stream_options: None,
            user: None,
            extra: serde_json::Map::new(),
        };

        resolve_thinking_and_effort(&req, &model, &mut openai_req);

        assert!(openai_req.thinking.is_some());
        assert_eq!(openai_req.thinking.unwrap()["type"], "enabled");
        assert_eq!(openai_req.reasoning_effort, Some("medium".to_owned()));
    }

    // -- transform_to_responses -----------------------------------------------

    #[test]
    fn responses_system_becomes_developer() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.system = Some(serde_json::Value::String("You are helpful".to_owned()));
        let model = make_model("gpt-4o");
        let result = transform_to_responses(&req, &model).unwrap();

        assert_eq!(result.input[0].role, "developer");
        assert_eq!(
            result.input[0].content,
            Some(serde_json::Value::String("You are helpful".to_owned()))
        );
    }

    #[test]
    fn responses_tools_transformed() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.tools = vec![crate::anthropic::Tool {
            name: "run_code".to_owned(),
            description: Some("Execute code".to_owned()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let model = make_model("gpt-4o");
        let result = transform_to_responses(&req, &model).unwrap();

        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].r#type, "function");
        assert_eq!(result.tools[0].name, Some("run_code".to_owned()));
    }

    #[test]
    fn responses_reasoning_from_config() {
        let req = make_request(vec![user_msg("hi")]);
        let mut model = make_model("gpt-4o");
        model.reasoning_effort = "high".to_owned();
        let result = transform_to_responses(&req, &model).unwrap();

        assert!(result.reasoning.is_some());
        assert_eq!(result.reasoning.unwrap().effort, Some("high".to_owned()));
    }

    // -- transform_to_gemini --------------------------------------------------

    #[test]
    fn gemini_system_prepended() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.system = Some(serde_json::Value::String("Be helpful".to_owned()));
        let model = make_model("gemini-2.5-pro");
        let result = transform_to_gemini(&req, &model).unwrap();

        assert_eq!(result.contents[0].role, "user");
        assert_eq!(
            result.contents[0].parts[0].text,
            Some("[System Instruction] Be helpful".to_owned())
        );
        assert_eq!(result.contents[1].role, "model");
        assert_eq!(
            result.contents[1].parts[0].text,
            Some("Understood. I will follow these instructions.".to_owned())
        );
    }

    #[test]
    fn gemini_assistant_uses_model_role() {
        let req = make_request(vec![user_msg("hi"), assistant_msg("hello")]);
        let model = make_model("gemini-2.5-pro");
        let result = transform_to_gemini(&req, &model).unwrap();

        assert_eq!(result.contents[0].role, "user");
        assert_eq!(result.contents[1].role, "model");
    }

    #[test]
    fn gemini_generation_config() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.max_tokens = 512;
        req.temperature = Some(0.7);
        let model = make_model("gemini-2.5-pro");
        let result = transform_to_gemini(&req, &model).unwrap();

        let gc = result.generation_config.unwrap();
        assert_eq!(gc.max_output_tokens, Some(512));
        assert_eq!(gc.temperature, Some(0.7));
    }

    #[test]
    fn gemini_model_temperature_overrides() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.temperature = Some(0.3);
        let mut model = make_model("gemini-2.5-pro");
        model.temperature = 0.8;
        let result = transform_to_gemini(&req, &model).unwrap();

        assert_eq!(result.generation_config.unwrap().temperature, Some(0.8));
    }

    #[test]
    fn gemini_tools_transformed() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.tools = vec![crate::anthropic::Tool {
            name: "search".to_owned(),
            description: Some("Search the web".to_owned()),
            input_schema: serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }];
        let model = make_model("gemini-2.5-pro");
        let result = transform_to_gemini(&req, &model).unwrap();

        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].function_declarations.len(), 1);
        assert_eq!(result.tools[0].function_declarations[0].name, "search");
    }

    #[test]
    fn gemini_tool_result_formatted() {
        let req = make_request(vec![user_msg_with_tool_result("tu_42", "result text")]);
        let model = make_model("gemini-2.5-pro");
        let result = transform_to_gemini(&req, &model).unwrap();

        assert_eq!(result.contents[0].role, "user");
        assert_eq!(
            result.contents[0].parts[0].text,
            Some("[Tool Result for tu_42] result text".to_owned())
        );
    }

    // -- transform_error_response ---------------------------------------------

    #[test]
    fn error_response_400() {
        let val = transform_error_response(400, "bad request");
        assert_eq!(val["error"]["type"], "invalid_request_error");
        assert_eq!(val["error"]["message"], "bad request");
    }

    #[test]
    fn error_response_401() {
        let val = transform_error_response(401, "bad key");
        assert_eq!(val["error"]["type"], "authentication_error");
    }

    #[test]
    fn error_response_403() {
        let val = transform_error_response(403, "denied");
        assert_eq!(val["error"]["type"], "permission_error");
    }

    #[test]
    fn error_response_404() {
        let val = transform_error_response(404, "missing");
        assert_eq!(val["error"]["type"], "not_found_error");
    }

    #[test]
    fn error_response_429() {
        let val = transform_error_response(429, "slow down");
        assert_eq!(val["error"]["type"], "rate_limit_error");
    }

    #[test]
    fn error_response_500() {
        let val = transform_error_response(500, "oops");
        assert_eq!(val["error"]["type"], "api_error");
    }

    #[test]
    fn error_response_503() {
        let val = transform_error_response(503, "unavailable");
        assert_eq!(val["error"]["type"], "api_error");
    }

    #[test]
    fn error_response_502() {
        let val = transform_error_response(502, "bad gateway");
        assert_eq!(val["error"]["type"], "api_error");
    }

    // -- has_thinking_blocks --------------------------------------------------

    #[test]
    fn thinking_blocks_detected() {
        let msgs = vec![assistant_msg_with_blocks(serde_json::json!([
            { "type": "thinking", "thinking": "hmm" },
            { "type": "text", "text": "answer" }
        ]))];
        assert!(has_thinking_blocks(&msgs));
    }

    #[test]
    fn thinking_blocks_in_tool_use() {
        let msgs = vec![assistant_msg_with_blocks(serde_json::json!([
            { "type": "tool_use", "id": "tu_1", "name": "f", "input": {}, "thinking": "reasoning" }
        ]))];
        assert!(has_thinking_blocks(&msgs));
    }

    #[test]
    fn no_thinking_blocks() {
        let msgs = vec![assistant_msg("plain text")];
        assert!(!has_thinking_blocks(&msgs));
    }

    // =========================================================================
    // Phase 0: Additional characterization tests
    // =========================================================================

    // -- transform_request: empty messages array ------------------------------

    /// Characterization: an empty messages array is passed through without error.
    /// The transformer produces an empty OpenAI messages array (just the system
    /// message if present, or nothing).
    #[test]
    fn transform_empty_messages_produces_empty_openai_messages() {
        let req = make_request(vec![]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        // No system prompt, no messages.
        assert!(result.messages.is_empty());
    }

    // -- transform_request: boundary values for max_tokens --------------------

    /// Characterization: max_tokens=0 means "do not set" (the transformer
    /// checks `if anthropic_req.max_tokens > 0`).
    #[test]
    fn transform_max_tokens_zero_not_set() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.max_tokens = 0;
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.max_tokens, None);
    }

    /// Characterization: negative max_tokens is treated as "do not set".
    #[test]
    fn transform_max_tokens_negative_not_set() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.max_tokens = -1;
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.max_tokens, None);
    }

    // -- budget_tokens_to_effort boundary values ------------------------------

    #[test]
    fn budget_tokens_boundary_0() {
        assert_eq!(budget_tokens_to_effort(0), "low");
    }

    #[test]
    fn budget_tokens_boundary_2048() {
        assert_eq!(budget_tokens_to_effort(2048), "low");
    }

    #[test]
    fn budget_tokens_boundary_2049() {
        assert_eq!(budget_tokens_to_effort(2049), "medium");
    }

    #[test]
    fn budget_tokens_boundary_8192() {
        assert_eq!(budget_tokens_to_effort(8192), "medium");
    }

    #[test]
    fn budget_tokens_boundary_8193() {
        assert_eq!(budget_tokens_to_effort(8193), "high");
    }

    #[test]
    fn budget_tokens_boundary_32768() {
        assert_eq!(budget_tokens_to_effort(32768), "high");
    }

    #[test]
    fn budget_tokens_boundary_32769() {
        assert_eq!(budget_tokens_to_effort(32769), "max");
    }

    #[test]
    fn budget_tokens_boundary_max() {
        assert_eq!(budget_tokens_to_effort(i64::MAX), "max");
    }

    // -- transform_request: minimal request (no optional fields) ---------------

    /// Characterization: a request with only required fields produces an
    /// OpenAI request where all optional fields are unset.
    #[test]
    fn transform_minimal_request_no_optional_fields() {
        let req = make_request(vec![user_msg("hi")]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.tools.len(), 0);
        assert_eq!(result.temperature, None);
        assert_eq!(result.top_p, None);
        assert_eq!(result.reasoning_effort, None);
        assert_eq!(result.thinking, None);
        assert!(result.stream_options.is_none());
    }

    // -- transform_request: tool_choice is forwarded --------------------------

    /// Characterization: tool_choice from the Anthropic request is mapped to
    /// the OpenAI request's tool_choice field.
    #[test]
    fn transform_tool_choice_forwarded() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.tools = vec![crate::anthropic::Tool {
            name: "get_weather".to_owned(),
            description: Some("Get the weather".to_owned()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }];
        req.tool_choice = Some(serde_json::json!({"type": "auto"}));
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();

        assert_eq!(result.tool_choice, Some(serde_json::json!({"type": "auto"})));
    }

    /// Characterization: tool_choice=None means no tool_choice in the OpenAI request.
    #[test]
    fn transform_no_tool_choice_when_absent() {
        let req = make_request(vec![user_msg("hi")]);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.tool_choice, None);
    }

    // -- transform_to_responses: tool result handling --------------------------

    /// Characterization: tool_result blocks in Anthropic messages become
    /// `tool` role inputs in the Responses API request.
    #[test]
    fn responses_tool_result_transformed() {
        let req = make_request(vec![
            assistant_msg_with_blocks(serde_json::json!([
                { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "SF"} }
            ])),
            user_msg_with_tool_result("tu_1", "72F and sunny"),
        ]);
        let model = make_model("gpt-4o");
        let result = transform_to_responses(&req, &model).unwrap();

        // Should have at least one tool role input.
        let tool_inputs: Vec<_> = result
            .input
            .iter()
            .filter(|i| i.role == "tool")
            .collect();
        assert_eq!(tool_inputs.len(), 1);
        assert_eq!(
            tool_inputs[0].content,
            Some(serde_json::Value::String("72F and sunny".to_owned()))
        );
    }

    // -- top_p forwarding ------------------------------------------------------

    /// Characterization: top_p from the Anthropic request is forwarded to the
    /// OpenAI request.
    #[test]
    fn transform_top_p_forwarded() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.top_p = Some(0.9);
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.top_p, Some(0.9));
    }

    // -- transform_error_response: boundary status codes -----------------------

    /// Characterization: status codes below 400 map to `api_error`.
    #[test]
    fn error_response_199_is_api_error() {
        let val = transform_error_response(199, "info");
        assert_eq!(val["error"]["type"], "api_error");
    }

    /// Characterization: status codes above 499 map to `api_error`.
    #[test]
    fn error_response_599_is_api_error() {
        let val = transform_error_response(599, "bad");
        assert_eq!(val["error"]["type"], "api_error");
    }

    // -- transform_tools: null schema gets default -----------------------------

    /// Characterization: a tool with `input_schema: Value::Null` produces a
    /// default `{"type": "object", "properties": {}}` schema.
    #[test]
    fn transform_tools_with_null_schema_gets_default() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.tools = vec![crate::anthropic::Tool {
            name: "my_tool".to_owned(),
            description: None,
            input_schema: serde_json::Value::Null,
        }];
        let model = make_model("glm-5.1");
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.tools.len(), 1);
        let params = result.tools[0].function.parameters.as_ref().unwrap();
        assert_eq!(params["type"], "object");
        assert!(params.get("properties").unwrap().as_object().unwrap().is_empty());
    }

    // -- max_tokens saturating cast -------------------------------------------

    /// Characterization: a max_tokens value exceeding i32::MAX is clamped to
    /// i32::MAX rather than wrapping to a negative number.
    #[test]
    fn transform_max_tokens_saturating_cast() {
        let mut req = make_request(vec![user_msg("hi")]);
        req.max_tokens = 0;
        let mut model = make_model("glm-5.1");
        model.max_tokens = i64::MAX;
        let result = transform_request(&req, &model).unwrap();
        assert_eq!(result.max_tokens, Some(i32::MAX));
    }

    // =========================================================================
    // Phase 0: Test inventory -- recorded fixture gaps
    // =========================================================================
    //
    // The following gaps are recorded per the plan's test inventory section.
    // Phase 2, Phase 5, and Phase 12 should add the right adapter fixtures:
    //
    // 1. Streaming tool calls for Responses API:
    //    `process_responses_chunk` does not handle
    //    `response.function_call_arguments.delta` events.
    //
    // 2. Streaming function calls for Gemini:
    //    `process_gemini_chunk` only handles text parts, not function call
    //    parts in the Gemini response.
    //
    // 3. Image content blocks:
    //    Images are replaced with `[Image]` placeholders. Full image
    //    passthrough requires core protocol support.
    //
    // 4. Redacted thinking blocks:
    //    The `redacted_thinking` content type is not modeled.
    //
    // 5. Refusal content types:
    //    OpenAI `refusal` fields on messages are not forwarded.
    //
    // 6. Snapshot/golden fixtures:
    //    No snapshot tests exist for any adapter. These should be added
    //    when the core protocol architecture is in place.
    //
    // 7. Disconnect behavior:
    //    No test simulates an actual client disconnect mid-stream (dropping
    //    the response body while SSE events are being written).
    //
    // 8. Upstream stream errors:
    //    No test simulates an HTTP 500 mid-stream from the upstream provider.
    //
    // Do not fill all fixture gaps in Phase 0. Record the gaps so later
    // phases add the right adapter fixtures.

    // -- Edge case tests for tool_use / tool_result with missing fields -----------

    /// Phase 0: tool_use block with missing 'id' field falls back to empty string.
    #[test]
    fn tool_use_missing_id_defaults_to_empty_string() {
        let messages = vec![Message {
            role: "assistant".to_owned(),
            content: serde_json::json!([
                {"type": "tool_use", "name": "my_tool", "input": {}}
            ]),
        }];
        let req = make_request(messages);
        let model = make_model("gpt-4o");
        let result = transform_request(&req, &model).unwrap();
        // The tool_use has no 'id', so get_tool_id() returns "".
        let tc = &result.messages[0].tool_calls[0];
        assert!(tc.id.is_none() || tc.id.as_deref() == Some(""));
    }

    /// Phase 0: tool_result block with missing 'tool_use_id' falls back to empty string.
    #[test]
    fn tool_result_missing_tool_use_id_defaults_to_empty_string() {
        let messages = vec![Message {
            role: "user".to_owned(),
            content: serde_json::json!([
                {"type": "tool_result", "content": "result text"}
            ]),
        }];
        let req = make_request(messages);
        let model = make_model("gpt-4o");
        let result = transform_request(&req, &model).unwrap();
        // tool_result without tool_use_id -> tool_call_id is Some("").
        assert_eq!(result.messages[0].tool_call_id.as_deref(), Some(""));
    }

    /// Phase 0: tool_use block with absent 'input' field defaults to "{}".
    #[test]
    fn tool_use_absent_input_defaults_to_empty_json() {
        let messages = vec![Message {
            role: "assistant".to_owned(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "tu_1", "name": "my_tool"}
            ]),
        }];
        let req = make_request(messages);
        let model = make_model("gpt-4o");
        let result = transform_request(&req, &model).unwrap();
        let tc = &result.messages[0].tool_calls[0];
        assert_eq!(tc.function.as_ref().unwrap().arguments.as_deref(), Some("{}"));
    }

    // -- Empty messages tests for Gemini and Responses ----------------------------

    /// Phase 0: transform_to_gemini with empty messages produces empty contents.
    #[test]
    fn gemini_empty_messages_produces_empty_contents() {
        let req = make_request(vec![]);
        let model = make_model("gemini-2.5-flash");
        let result = transform_to_gemini(&req, &model).unwrap();
        assert!(result.contents.is_empty());
    }

    /// Phase 0: transform_to_responses with empty messages produces empty inputs.
    #[test]
    fn responses_empty_messages_produces_empty_inputs() {
        let req = make_request(vec![]);
        let model = make_model("gpt-4o");
        let result = transform_to_responses(&req, &model).unwrap();
        assert!(result.input.is_empty());
    }
}
