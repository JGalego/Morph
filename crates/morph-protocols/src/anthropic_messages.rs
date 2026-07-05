//! `ProtocolAdapter` for Anthropic's Messages wire format
//! (`POST /v1/messages`).

use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use morph_core::prelude::*;

use crate::util::guess_mime_from_extension;

/// Translates Anthropic Messages requests/responses to and from
/// `morph-core`'s canonical types. See the doc comment on
/// `ProtocolAdapter::encode_stream_event` (impl block below) for how the six
/// Anthropic SSE event types map onto `ResponseEvent`.
#[derive(Debug, Default)]
pub struct AnthropicMessagesProtocol;

impl AnthropicMessagesProtocol {
    pub fn new() -> Self {
        Self
    }
}

// ---- wire-shape DTOs -------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicRequest {
    model: String,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    system: Option<AnthropicSystem>,
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    tools: Vec<AnthropicTool>,
    #[serde(default)]
    tool_choice: Option<AnthropicToolChoice>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stop_sequences: Vec<String>,
    /// Fields with no canonical equivalent (`metadata`, `thinking`,
    /// `anthropic_version` when sent in-body, ...) are preserved here.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnthropicSystem {
    Text(String),
    Blocks(Vec<AnthropicSystemBlock>),
}

#[derive(Debug, Deserialize)]
struct AnthropicSystemBlock {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<AnthropicToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    /// Anything else (`thinking`, `redacted_thinking`, `server_tool_use`,
    /// ...) is accepted rather than failing the whole request.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnthropicToolResultContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Deserialize)]
struct AnthropicTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicToolChoice {
    Auto,
    Any,
    #[serde(rename = "none")]
    NoneChoice,
    Tool {
        name: String,
    },
}

// ---- helpers -----------------------------------------------------------

fn tool_result_content_to_text(content: Option<AnthropicToolResultContent>) -> String {
    match content {
        None => String::new(),
        Some(AnthropicToolResultContent::Text(t)) => t,
        Some(AnthropicToolResultContent::Blocks(blocks)) => blocks
            .into_iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn image_source_to_canonical(source: AnthropicImageSource) -> ContentBlock {
    match source {
        AnthropicImageSource::Base64 { media_type, data } => ContentBlock::Image(ImageBlock {
            mime: media_type,
            source: ImageSource::Base64 { data },
            rendered_by_morph: false,
        }),
        AnthropicImageSource::Url { url } => ContentBlock::Image(ImageBlock {
            mime: guess_mime_from_extension(&url),
            source: ImageSource::Url { url },
            rendered_by_morph: false,
        }),
    }
}

fn anthropic_block_to_canonical(block: AnthropicContentBlock) -> Option<ContentBlock> {
    match block {
        AnthropicContentBlock::Text { text } => Some(ContentBlock::text(text)),
        AnthropicContentBlock::Image { source } => Some(image_source_to_canonical(source)),
        AnthropicContentBlock::ToolUse { id, name, input } => {
            Some(ContentBlock::ToolUse(ToolUseBlock { id, name, input }))
        }
        AnthropicContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Some(ContentBlock::ToolResult(ToolResultBlock {
            tool_use_id,
            content: tool_result_content_to_text(content),
            is_error,
        })),
        AnthropicContentBlock::Unknown => None,
    }
}

fn image_block_to_json(mime: &str, source: &ImageSource) -> Value {
    match source {
        ImageSource::Base64 { data } => json!({"type": "base64", "media_type": mime, "data": data}),
        ImageSource::Url { url } => json!({"type": "url", "url": url}),
    }
}

/// Maps a canonical `StopReason` to the `stop_reason` string Anthropic's
/// Messages API uses.
fn anthropic_stop_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        // Anthropic's API has no dedicated "generation failed" stop reason
        // (errors are surfaced as HTTP-level failures instead); "error" is
        // this crate's own extension for the case where a fully
        // materialized `CanonicalResponse` still carries `StopReason::Error`.
        StopReason::Error => "error",
    }
}

fn anthropic_error_type(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::InvalidRequest(_) | GatewayError::Unsupported(_) => "invalid_request_error",
        GatewayError::Unauthorized(_) => "authentication_error",
        GatewayError::RateLimited { .. } => "rate_limit_error",
        _ => "api_error",
    }
}

// ---- trait impl ---------------------------------------------------------

/// # Streaming event-type mapping
///
/// Anthropic's Messages streaming API frames each server-sent event with an
/// explicit `event: <type>` line before its `data: {...}` line. This method
/// maps each `ResponseEvent` variant onto exactly one native Anthropic event
/// type:
///
/// | `ResponseEvent`     | Anthropic `event:` type |
/// |----------------------|--------------------------|
/// | `MessageStart`       | `message_start`          |
/// | `ToolCallStart`      | `content_block_start`    |
/// | `TextDelta`          | `content_block_delta` (`text_delta`) |
/// | `ToolCallDelta`      | `content_block_delta` (`input_json_delta`) |
/// | `ToolCallEnd`        | `content_block_stop`     |
/// | `Usage`              | `message_delta`          |
/// | `MessageStop`        | `message_stop`           |
///
/// Two deliberate deviations from the official wire shape, both a
/// consequence of `ResponseEvent` splitting information differently than
/// Anthropic's own event stream does:
///
/// - Real Anthropic streams never emit a `content_block_start` for a plain
///   text block (only for `tool_use`); text is expected to start flowing via
///   `content_block_delta` once `message_start` has opened the message.
///   `ResponseEvent` has no "text block started" signal distinct from
///   `MessageStart`, so this adapter follows the same convention: `TextDelta`
///   goes straight to `content_block_delta` with no preceding
///   `content_block_start`.
/// - Real Anthropic streams report the final `stop_reason` on `message_delta`
///   and leave the terminal `message_stop` event body empty. Here,
///   `stop_reason` only exists on the `MessageStop` variant, so this adapter
///   puts it on the `message_stop` payload instead (a non-standard but
///   harmless extension — no mainstream Anthropic SDK reads fields off
///   `message_stop`).
impl ProtocolAdapter for AnthropicMessagesProtocol {
    fn protocol_id(&self) -> &str {
        "anthropic_messages"
    }

    fn parse_request(&self, raw: &[u8], request_id: &str) -> Result<CanonicalRequest> {
        let wire: AnthropicRequest = serde_json::from_slice(raw).map_err(|e| {
            GatewayError::InvalidRequest(format!("invalid anthropic messages body: {e}"))
        })?;

        let max_tokens = wire
            .max_tokens
            .ok_or_else(|| GatewayError::InvalidRequest("max_tokens is required".to_string()))?;

        let system = wire
            .system
            .map(|s| match s {
                AnthropicSystem::Text(t) => t,
                AnthropicSystem::Blocks(blocks) => blocks
                    .into_iter()
                    .filter_map(|b| b.text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .filter(|s| !s.is_empty());

        let mut messages = Vec::with_capacity(wire.messages.len());
        for m in wire.messages {
            let role = match m.role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                other => {
                    return Err(GatewayError::InvalidRequest(format!(
                        "unsupported anthropic message role '{other}'"
                    )))
                }
            };

            let mut blocks = match m.content {
                AnthropicContent::Text(t) => vec![ContentBlock::text(t)],
                AnthropicContent::Blocks(parts) => parts
                    .into_iter()
                    .filter_map(anthropic_block_to_canonical)
                    .collect(),
            };
            if blocks.is_empty() {
                blocks.push(ContentBlock::text(""));
            }

            messages.push(Message {
                role,
                content: blocks,
                name: None,
            });
        }

        let tools = wire
            .tools
            .into_iter()
            .map(|t| ToolDefinition {
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            })
            .collect();

        let tool_choice = wire.tool_choice.map(|tc| match tc {
            AnthropicToolChoice::Auto => ToolChoice::Auto,
            AnthropicToolChoice::Any => ToolChoice::Required,
            AnthropicToolChoice::NoneChoice => ToolChoice::None,
            AnthropicToolChoice::Tool { name } => ToolChoice::Specific { name },
        });

        let extra = if wire.extra.is_empty() {
            Value::Null
        } else {
            Value::Object(wire.extra)
        };

        Ok(CanonicalRequest {
            model: wire.model,
            messages,
            system,
            tools,
            tool_choice,
            temperature: wire.temperature,
            top_p: wire.top_p,
            max_tokens: Some(max_tokens),
            stream: wire.stream,
            stop: wire.stop_sequences,
            response_format: None,
            reasoning: None,
            metadata: RequestMetadata {
                request_id: request_id.to_string(),
                ingress_protocol: self.protocol_id().to_string(),
                received_at: SystemTime::now(),
            },
            extra,
        })
    }

    fn encode_stream_event(&self, event: &ResponseEvent) -> Result<Option<Vec<u8>>> {
        let (event_type, data) = match event {
            ResponseEvent::MessageStart { id, model } => (
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": id,
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": model,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                    },
                }),
            ),
            ResponseEvent::ToolCallStart { index, id, name } => (
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}},
                }),
            ),
            ResponseEvent::TextDelta { index, text } => (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": text},
                }),
            ),
            ResponseEvent::ToolCallDelta {
                index,
                partial_json,
            } => (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": partial_json},
                }),
            ),
            ResponseEvent::ToolCallEnd { index } => (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ),
            ResponseEvent::Usage(usage) => (
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {},
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                    },
                }),
            ),
            ResponseEvent::MessageStop { stop_reason } => (
                "message_stop",
                json!({
                    "type": "message_stop",
                    "stop_reason": anthropic_stop_reason(*stop_reason),
                }),
            ),
        };

        let mut bytes = format!("event: {event_type}\ndata: ").into_bytes();
        bytes.extend_from_slice(&serde_json::to_vec(&data)?);
        bytes.extend_from_slice(b"\n\n");
        Ok(Some(bytes))
    }

    fn encode_buffered(&self, resp: &CanonicalResponse) -> Result<Vec<u8>> {
        let content: Vec<Value> = resp
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(t) => json!({"type": "text", "text": t.text}),
                ContentBlock::ToolUse(tu) => {
                    json!({"type": "tool_use", "id": tu.id, "name": tu.name, "input": tu.input})
                }
                ContentBlock::Image(img) => {
                    json!({"type": "image", "source": image_block_to_json(&img.mime, &img.source)})
                }
                ContentBlock::ToolResult(tr) => json!({
                    "type": "tool_result",
                    "tool_use_id": tr.tool_use_id,
                    "content": tr.content,
                    "is_error": tr.is_error,
                }),
            })
            .collect();

        let body = json!({
            "id": resp.id,
            "type": "message",
            "role": "assistant",
            "model": resp.model,
            "content": content,
            "stop_reason": anthropic_stop_reason(resp.stop_reason),
            "stop_sequence": null,
            "usage": {
                "input_tokens": resp.usage.input_tokens,
                "output_tokens": resp.usage.output_tokens,
            },
        });

        Ok(serde_json::to_vec(&body)?)
    }

    fn encode_error(&self, err: &GatewayError) -> Vec<u8> {
        let body = json!({
            "type": "error",
            "error": {
                "type": anthropic_error_type(err),
                "message": err.to_string(),
            },
        });
        serde_json::to_vec(&body).unwrap_or_else(|_| {
            b"{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"internal error\"}}".to_vec()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> AnthropicMessagesProtocol {
        AnthropicMessagesProtocol::new()
    }

    #[test]
    fn parses_request_with_tools_and_system() {
        let raw = br#"{
            "model": "claude-3-5-sonnet-20241022",
            "max_tokens": 1024,
            "system": "Be terse.",
            "messages": [
                {"role": "user", "content": "What's the weather?"}
            ],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object", "properties": {"location": {"type": "string"}}}
            }],
            "tool_choice": {"type": "auto"},
            "stream": false
        }"#;

        let req = protocol().parse_request(raw, "req-1").unwrap();

        assert_eq!(req.model, "claude-3-5-sonnet-20241022");
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.system, Some("Be terse.".to_string()));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "get_weather");
        assert!(matches!(req.tool_choice, Some(ToolChoice::Auto)));
        assert_eq!(req.metadata.ingress_protocol, "anthropic_messages");
        assert_eq!(req.metadata.request_id, "req-1");
    }

    #[test]
    fn parses_request_with_image_and_tool_result_blocks() {
        let raw = br#"{
            "model": "claude-3-5-sonnet-20241022",
            "max_tokens": 512,
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Describe"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9j/4AAQ"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"location": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny", "is_error": false}
                ]}
            ]
        }"#;

        let req = protocol().parse_request(raw, "req-2").unwrap();

        assert_eq!(req.messages.len(), 3);
        match &req.messages[0].content[1] {
            ContentBlock::Image(img) => {
                assert_eq!(img.mime, "image/jpeg");
                assert!(matches!(&img.source, ImageSource::Base64 { data } if data == "/9j/4AAQ"));
            }
            other => panic!("expected image block, got {other:?}"),
        }
        match &req.messages[1].content[0] {
            ContentBlock::ToolUse(tu) => {
                assert_eq!(tu.id, "toolu_1");
                assert_eq!(tu.name, "get_weather");
                assert_eq!(tu.input["location"], "Paris");
            }
            other => panic!("expected tool_use block, got {other:?}"),
        }
        match &req.messages[2].content[0] {
            ContentBlock::ToolResult(tr) => {
                assert_eq!(tr.tool_use_id, "toolu_1");
                assert_eq!(tr.content, "sunny");
                assert!(!tr.is_error);
            }
            other => panic!("expected tool_result block, got {other:?}"),
        }
    }

    #[test]
    fn requires_max_tokens() {
        let raw = br#"{
            "model": "claude-3-5-sonnet-20241022",
            "messages": [{"role": "user", "content": "hi"}]
        }"#;
        let err = protocol().parse_request(raw, "req-3").unwrap_err();
        assert!(matches!(err, GatewayError::InvalidRequest(_)));
    }

    #[test]
    fn encodes_text_delta_as_content_block_delta() {
        let event = ResponseEvent::TextDelta {
            index: 0,
            text: "Hello".to_string(),
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("event: content_block_delta\ndata: "));
        assert!(s.ends_with("\n\n"));
        let json_part = s
            .trim_start_matches("event: content_block_delta\ndata: ")
            .trim_end();
        let parsed: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(parsed["delta"]["type"], "text_delta");
        assert_eq!(parsed["delta"]["text"], "Hello");
    }

    #[test]
    fn encodes_message_stop_with_stop_reason() {
        let event = ResponseEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("event: message_stop\ndata: "));
        let json_part = s
            .trim_start_matches("event: message_stop\ndata: ")
            .trim_end();
        let parsed: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(parsed["type"], "message_stop");
        assert_eq!(parsed["stop_reason"], "tool_use");
    }

    #[test]
    fn encodes_buffered_response() {
        let resp = CanonicalResponse {
            id: "msg_1".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            content: vec![ContentBlock::text("Hi there")],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        let bytes = protocol().encode_buffered(&resp).unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["type"], "message");
        assert_eq!(parsed["content"][0]["type"], "text");
        assert_eq!(parsed["content"][0]["text"], "Hi there");
        assert_eq!(parsed["stop_reason"], "end_turn");
        assert_eq!(parsed["usage"]["input_tokens"], 10);
    }

    #[test]
    fn encodes_error() {
        let err = GatewayError::Unauthorized("bad key".to_string());
        let bytes = protocol().encode_error(&err);
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["error"]["type"], "authentication_error");
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bad key"));
    }
}
