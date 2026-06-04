//! Anthropic API types for the Messages API.
//!
//! Reference: <https://docs.anthropic.com/en/api/messages>
//!
//! This module contains request/response types for the Anthropic Messages API,
//! including support for streaming via Server-Sent Events.

use serde::{Deserialize, Serialize, Serializer};

// ---------------------------------------------------------------------------
// MessageRequest
// ---------------------------------------------------------------------------

/// A request to the Anthropic Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRequest {
    /// The model identifier (e.g. "claude-sonnet-4-20250514").
    pub model: String,
    /// Maximum number of tokens to generate.
    pub max_tokens: i32,
    /// Optional system prompt. Can be a plain string or an array of [`SystemContentBlock`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<serde_json::Value>,
    /// The conversation messages.
    pub messages: Vec<Message>,
    /// Whether to stream the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Tool definitions available to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Optional metadata attached to the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    /// Extended thinking configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
}

impl MessageRequest {
    /// Extracts the system prompt text from the `system` field.
    ///
    /// Anthropic accepts `system` as either a plain string or an array of
    /// [`SystemContentBlock`]:
    ///
    /// ```json
    /// "system": "You are helpful"
    /// "system": [{"type":"text","text":"You are helpful","cache_control":...}]
    /// ```
    pub fn system_text(&self) -> String {
        match &self.system {
            None => String::new(),
            Some(value) => {
                // Try plain string first.
                if let Some(s) = value.as_str() {
                    return s.to_owned();
                }
                // Try array of SystemContentBlock.
                if let Some(arr) = value.as_array() {
                    let mut text = String::new();
                    for item in arr {
                        if let Ok(block) =
                            serde_json::from_value::<SystemContentBlock>(item.clone())
                        {
                            if block.r#type == "text" {
                                if let Some(t) = block.text {
                                    text.push_str(&t);
                                }
                            }
                        }
                    }
                    if !text.is_empty() {
                        return text;
                    }
                }
                // Fallback: raw JSON string.
                value.to_string()
            }
        }
    }

    /// Validates that required fields are present.
    ///
    /// Returns an error message if `model` is empty or `messages` is empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.model.is_empty() {
            return Err("model is required".to_owned());
        }
        if self.messages.is_empty() {
            return Err("messages is required".to_owned());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SystemContentBlock
// ---------------------------------------------------------------------------

/// A content block inside the `system` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemContentBlock {
    /// Block type, typically `"text"`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// The text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Cache control directive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ---------------------------------------------------------------------------
// CacheControl
// ---------------------------------------------------------------------------

/// Cache control directives for prompt caching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    /// Cache control type, typically `"ephemeral"`.
    #[serde(rename = "type")]
    pub r#type: String,
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Optional metadata attached to a [`MessageRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// An external identifier for the end-user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single message in the conversation.
///
/// The `content` field can be either a plain string or an array of
/// [`ContentBlock`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message author (`"user"` or `"assistant"`).
    pub role: String,
    /// Message content – a string or array of content blocks.
    pub content: serde_json::Value,
}

impl Message {
    /// Parses the message `content` into a list of [`ContentBlock`].
    ///
    /// Handles both a plain string and an array-of-blocks representation:
    ///
    /// ```json
    /// "content": "hello"
    /// "content": [{"type":"text","text":"hello"}]
    /// ```
    pub fn content_blocks(&self) -> Vec<ContentBlock> {
        if self.content.is_null() {
            return Vec::new();
        }
        // Try plain string first.
        if let Some(s) = self.content.as_str() {
            return vec![ContentBlock {
                r#type: "text".to_owned(),
                text: Some(s.to_owned()),
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
            }];
        }
        // Try array of content blocks.
        if let Some(arr) = self.content.as_array() {
            let mut blocks = Vec::with_capacity(arr.len());
            for item in arr {
                if let Ok(b) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    blocks.push(b);
                }
            }
            return blocks;
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// ContentBlock
// ---------------------------------------------------------------------------

/// A block of content within a message.
///
/// The `r#type` field determines which other fields are populated:
///
/// | Type           | Populated fields                                  |
/// |----------------|---------------------------------------------------|
/// | `"text"`       | `text`                                            |
/// | `"tool_use"`   | `id`, `name`, `input`                             |
/// | `"tool_result"`| `tool_use_id`, `content`, `is_error`              |
/// | `"thinking"`   | `thinking`, `signature`                           |
/// | `"image"`      | `source`                                          |
#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlock {
    /// Block type discriminator.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Text content (for `"text"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool call identifier (for `"tool_use"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tool call ID this result refers to (for `"tool_result"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Tool name (for `"tool_use"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool input (for `"tool_use"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// Deprecated: use `content` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// Inner content for `"tool_result"` blocks (string or array).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    /// Whether the tool result is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Thinking text (for `"thinking"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Signature for the thinking block (extended thinking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Image source (for `"image"` blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ImageSource>,
}

impl ContentBlock {
    /// Returns the appropriate tool ID for this block.
    ///
    /// For `"tool_result"` blocks returns `tool_use_id`; for all others
    /// (notably `"tool_use"`) returns `id`.
    pub fn get_tool_id(&self) -> String {
        if self.r#type == "tool_result" {
            self.tool_use_id.clone().unwrap_or_default()
        } else {
            self.id.clone().unwrap_or_default()
        }
    }

    /// Extracts text from a `"tool_result"` block's `content` field.
    ///
    /// The `content` field can be a plain string or an array of content
    /// blocks. Falls back to the deprecated `output` field.
    pub fn text_content(&self) -> String {
        // Try the content field first.
        if let Some(ref val) = self.content {
            if let Some(s) = val.as_str() {
                return s.to_owned();
            }
            if let Some(arr) = val.as_array() {
                let mut text = String::new();
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                }
                if !text.is_empty() {
                    return text;
                }
            }
        }
        // Fallback to the deprecated output field.
        if let Some(ref val) = self.output {
            if let Some(s) = val.as_str() {
                return s.to_owned();
            }
            return val.to_string();
        }
        String::new()
    }
}

/// Custom [`Serialize`] for [`ContentBlock`] that emits only the fields
/// relevant to each block type, matching the strict Anthropic API schema.
impl Serialize for ContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;

        match self.r#type.as_str() {
            "text" => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", &self.r#type)?;
                map.serialize_entry("text", self.text.as_deref().unwrap_or(""))?;
                map.end()
            }
            "tool_use" => {
                let input = self
                    .input
                    .as_ref()
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", &self.r#type)?;
                map.serialize_entry("id", self.id.as_deref().unwrap_or(""))?;
                map.serialize_entry("name", self.name.as_deref().unwrap_or(""))?;
                map.serialize_entry("input", &input)?;
                map.end()
            }
            "tool_result" => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", &self.r#type)?;
                map.serialize_entry("tool_use_id", self.tool_use_id.as_deref().unwrap_or(""))?;
                if let Some(ref content) = self.content {
                    map.serialize_entry("content", content)?;
                }
                if let Some(is_error) = self.is_error {
                    map.serialize_entry("is_error", &is_error)?;
                }
                map.end()
            }
            "thinking" => {
                let has_sig = self.signature.is_some();
                let len = if has_sig { 3 } else { 2 };
                let mut map = serializer.serialize_map(Some(len))?;
                map.serialize_entry("type", &self.r#type)?;
                map.serialize_entry("thinking", self.thinking.as_deref().unwrap_or(""))?;
                if let Some(ref sig) = self.signature {
                    map.serialize_entry("signature", sig)?;
                }
                map.end()
            }
            "image" => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", &self.r#type)?;
                map.serialize_entry(
                    "source",
                    self.source.as_ref().unwrap_or(&ImageSource {
                        r#type: String::new(),
                        media_type: String::new(),
                        data: String::new(),
                    }),
                )?;
                map.end()
            }
            // Unknown / future block types: serialize all fields.
            _ => {
                #[derive(Serialize)]
                struct AllFields {
                    #[serde(rename = "type")]
                    r#type: String,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    text: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    id: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    tool_use_id: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    name: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    input: Option<serde_json::Value>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    output: Option<serde_json::Value>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    content: Option<serde_json::Value>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    is_error: Option<bool>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    thinking: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    signature: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    source: Option<ImageSource>,
                }

                let all = AllFields {
                    r#type: self.r#type.clone(),
                    text: self.text.clone(),
                    id: self.id.clone(),
                    tool_use_id: self.tool_use_id.clone(),
                    name: self.name.clone(),
                    input: self.input.clone(),
                    output: self.output.clone(),
                    content: self.content.clone(),
                    is_error: self.is_error,
                    thinking: self.thinking.clone(),
                    signature: self.signature.clone(),
                    source: self.source.clone(),
                };
                all.serialize(serializer)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ImageSource
// ---------------------------------------------------------------------------

/// An image source embedded in a content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    /// Source type, typically `"base64"`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// MIME type of the image (e.g. `"image/png"`).
    pub media_type: String,
    /// Base64-encoded image data.
    pub data: String,
}

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// A tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// The tool name.
    pub name: String,
    /// Human-readable description of what the tool does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// ToolResult
// ---------------------------------------------------------------------------

/// The result of executing a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The tool call ID this result corresponds to.
    pub tool_use_id: String,
    /// The result content.
    pub content: String,
    /// Whether the tool execution resulted in an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

// ---------------------------------------------------------------------------
// MessageResponse
// ---------------------------------------------------------------------------

/// A response from the Anthropic Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    /// Unique message identifier.
    pub id: String,
    /// Response type, always `"message"`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Conversation role, always `"assistant"`.
    pub role: String,
    /// The content blocks generated by the model.
    pub content: Vec<ContentBlock>,
    /// The model that produced this response.
    pub model: String,
    /// The reason generation stopped (e.g. `"end_turn"`, `"max_tokens"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// The stop sequence that caused generation to stop, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    /// Token usage statistics.
    pub usage: Usage,
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// Token usage statistics returned with every response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Number of tokens in the input prompt.
    pub input_tokens: i32,
    /// Number of tokens in the generated output.
    pub output_tokens: i32,
    /// Tokens used to create a new cache entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i32>,
    /// Tokens read from an existing cache entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<i32>,
}

// ---------------------------------------------------------------------------
// ContentBlockDelta
// ---------------------------------------------------------------------------

/// A streaming delta event for a content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlockDelta {
    /// Event type, always `"content_block_delta"`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Index of the content block this delta applies to.
    pub index: usize,
    /// The partial update.
    pub delta: Delta,
}

// ---------------------------------------------------------------------------
// Delta
// ---------------------------------------------------------------------------

/// A partial update in a streaming response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    /// Delta type (e.g. `"text_delta"`, `"thinking_delta"`, `"input_json_delta"`).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Partial text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Partial thinking content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Partial JSON for tool input streaming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_json: Option<String>,
    /// Stop reason (on the final delta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// MessageEvent
// ---------------------------------------------------------------------------

/// A Server-Sent Event from the Anthropic streaming API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEvent {
    /// Event type (e.g. `"message_start"`, `"content_block_start"`,
    /// `"content_block_delta"`, `"message_stop"`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// The full message (present on `"message_start"` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageResponse>,
    /// Content block index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// A content block (present on `"content_block_start"` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_block: Option<ContentBlock>,
    /// A streaming delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Delta>,
    /// Token usage (present on `"message_delta"` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// An error from the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

// ---------------------------------------------------------------------------
// ApiError
// ---------------------------------------------------------------------------

/// An error returned by the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// Error type (e.g. `"invalid_request_error"`).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Human-readable error description.
    pub message: String,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- system_text --------------------------------------------------------

    #[test]
    fn system_text_none_when_absent() {
        let req = MessageRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 1024,
            system: None,
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        assert_eq!(req.system_text(), "");
    }

    #[test]
    fn system_text_plain_string() {
        let req = MessageRequest {
            model: "test".into(),
            max_tokens: 100,
            system: Some(serde_json::Value::String("You are helpful".into())),
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        assert_eq!(req.system_text(), "You are helpful");
    }

    #[test]
    fn system_text_array_of_blocks() {
        let raw = serde_json::json!([
            { "type": "text", "text": "You are helpful. " },
            { "type": "text", "text": "Be concise." }
        ]);
        let req = MessageRequest {
            model: "test".into(),
            max_tokens: 100,
            system: Some(raw),
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        assert_eq!(req.system_text(), "You are helpful. Be concise.");
    }

    // -- content_blocks -----------------------------------------------------

    #[test]
    fn content_blocks_from_string() {
        let msg = Message {
            role: "user".into(),
            content: serde_json::Value::String("hello world".into()),
        };
        let blocks = msg.content_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].r#type, "text");
        assert_eq!(blocks[0].text.as_deref(), Some("hello world"));
    }

    #[test]
    fn content_blocks_from_array() {
        let raw = serde_json::json!([
            { "type": "text", "text": "hello" },
            { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city":"SF"} }
        ]);
        let msg = Message {
            role: "assistant".into(),
            content: raw,
        };
        let blocks = msg.content_blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].r#type, "text");
        assert_eq!(blocks[0].text.as_deref(), Some("hello"));
        assert_eq!(blocks[1].r#type, "tool_use");
        assert_eq!(blocks[1].id.as_deref(), Some("tu_1"));
        assert_eq!(blocks[1].name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn content_blocks_null_returns_empty() {
        let msg = Message {
            role: "user".into(),
            content: serde_json::Value::Null,
        };
        assert!(msg.content_blocks().is_empty());
    }

    // -- validate -----------------------------------------------------------

    #[test]
    fn validate_ok() {
        let req = MessageRequest {
            model: "claude-3".into(),
            max_tokens: 100,
            system: None,
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn validate_missing_model() {
        let req = MessageRequest {
            model: String::new(),
            max_tokens: 100,
            system: None,
            messages: vec![Message {
                role: "user".into(),
                content: serde_json::Value::String("hi".into()),
            }],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err, "model is required");
    }

    #[test]
    fn validate_missing_messages() {
        let req = MessageRequest {
            model: "claude-3".into(),
            max_tokens: 100,
            system: None,
            messages: vec![],
            stream: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            metadata: None,
            thinking: None,
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err, "messages is required");
    }

    // -- ContentBlock custom serialize --------------------------------------

    #[test]
    fn serialize_text_block() {
        let block = ContentBlock {
            r#type: "text".into(),
            text: Some("hello".into()),
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
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
        assert_eq!(json.as_object().unwrap().len(), 2);
    }

    #[test]
    fn serialize_tool_use_block() {
        let block = ContentBlock {
            r#type: "tool_use".into(),
            text: None,
            id: Some("tu_1".into()),
            tool_use_id: None,
            name: Some("get_weather".into()),
            input: Some(serde_json::json!({"city": "SF"})),
            output: None,
            content: None,
            is_error: None,
            thinking: None,
            signature: None,
            source: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "tu_1");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["input"]["city"], "SF");
        assert_eq!(json.as_object().unwrap().len(), 4);
    }

    #[test]
    fn serialize_tool_result_block() {
        let block = ContentBlock {
            r#type: "tool_result".into(),
            text: None,
            id: None,
            tool_use_id: Some("tu_1".into()),
            name: None,
            input: None,
            output: None,
            content: Some(serde_json::json!("72F and sunny")),
            is_error: Some(false),
            thinking: None,
            signature: None,
            source: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "tu_1");
        assert_eq!(json["content"], "72F and sunny");
        assert_eq!(json["is_error"], false);
    }

    #[test]
    fn serialize_thinking_block() {
        let block = ContentBlock {
            r#type: "thinking".into(),
            text: None,
            id: None,
            tool_use_id: None,
            name: None,
            input: None,
            output: None,
            content: None,
            is_error: None,
            thinking: Some("hmm...".into()),
            signature: Some("sig_abc".into()),
            source: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["thinking"], "hmm...");
        assert_eq!(json["signature"], "sig_abc");
        assert_eq!(json.as_object().unwrap().len(), 3);
    }

    // -- text_content -------------------------------------------------------

    #[test]
    fn text_content_from_string_content() {
        let block = ContentBlock {
            r#type: "tool_result".into(),
            text: None,
            id: None,
            tool_use_id: Some("tu_1".into()),
            name: None,
            input: None,
            output: None,
            content: Some(serde_json::Value::String("result text".into())),
            is_error: None,
            thinking: None,
            signature: None,
            source: None,
        };
        assert_eq!(block.text_content(), "result text");
    }

    #[test]
    fn text_content_from_array_content() {
        let raw = serde_json::json!([
            { "type": "text", "text": "part one" },
            { "type": "text", "text": "part two" }
        ]);
        let block = ContentBlock {
            r#type: "tool_result".into(),
            text: None,
            id: None,
            tool_use_id: Some("tu_1".into()),
            name: None,
            input: None,
            output: None,
            content: Some(raw),
            is_error: None,
            thinking: None,
            signature: None,
            source: None,
        };
        assert_eq!(block.text_content(), "part onepart two");
    }

    #[test]
    fn text_content_fallback_to_output() {
        let block = ContentBlock {
            r#type: "tool_result".into(),
            text: None,
            id: None,
            tool_use_id: Some("tu_1".into()),
            name: None,
            input: None,
            output: Some(serde_json::Value::String("legacy output".into())),
            content: None,
            is_error: None,
            thinking: None,
            signature: None,
            source: None,
        };
        assert_eq!(block.text_content(), "legacy output");
    }

    // -- get_tool_id --------------------------------------------------------

    #[test]
    fn get_tool_id_tool_result() {
        let block = ContentBlock {
            r#type: "tool_result".into(),
            tool_use_id: Some("tu_42".into()),
            ..make_minimal_block()
        };
        assert_eq!(block.get_tool_id(), "tu_42");
    }

    #[test]
    fn get_tool_id_tool_use() {
        let block = ContentBlock {
            r#type: "tool_use".into(),
            id: Some("tu_99".into()),
            ..make_minimal_block()
        };
        assert_eq!(block.get_tool_id(), "tu_99");
    }

    // -- helper to build a minimal block for tests --------------------------

    fn make_minimal_block() -> ContentBlock {
        ContentBlock {
            r#type: String::new(),
            text: None,
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
}
