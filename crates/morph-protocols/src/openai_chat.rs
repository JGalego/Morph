//! `ProtocolAdapter` for OpenAI's Chat Completions wire format
//! (`POST /v1/chat/completions`).

use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use morph_core::prelude::*;

use crate::util::{guess_mime_from_extension, parse_data_url};

/// Translates OpenAI Chat Completions requests/responses to and from
/// `morph-core`'s canonical types. See the doc comment on
/// `ProtocolAdapter::encode_stream_event` (impl block below) for the
/// streaming-framing contract this adapter follows.
#[derive(Debug, Default)]
pub struct OpenAiChatProtocol;

impl OpenAiChatProtocol {
    pub fn new() -> Self {
        Self
    }
}

// ---- wire-shape DTOs -------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(default)]
    tools: Vec<OpenAiTool>,
    #[serde(default)]
    tool_choice: Option<OpenAiToolChoice>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stop: Option<OpenAiStop>,
    #[serde(default)]
    response_format: Option<OpenAiResponseFormat>,
    /// Every field OpenAI's request supports that has no canonical
    /// equivalent (`n`, `presence_penalty`, `seed`, `user`, ...) lands here
    /// instead of being silently dropped.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(default)]
    content: Option<OpenAiContent>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiContentPart {
    Text {
        text: String,
    },
    ImageUrl {
        image_url: OpenAiImageUrl,
    },
    /// Anything else (`input_audio`, `refusal`, ...) is accepted rather than
    /// failing the whole request, since this crate only promises to render
    /// text + image content faithfully.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageUrl {
    url: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiFunctionDef,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDef {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiToolChoice {
    Mode(String),
    Specific { function: OpenAiToolChoiceFunction },
}

#[derive(Debug, Deserialize)]
struct OpenAiToolChoiceFunction {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiStop {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseFormat {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    json_schema: Option<Value>,
}

// ---- helpers -----------------------------------------------------------

fn content_to_plain_text(content: &Option<OpenAiContent>) -> Option<String> {
    match content {
        Some(OpenAiContent::Text(t)) => Some(t.clone()),
        Some(OpenAiContent::Parts(parts)) => Some(
            parts
                .iter()
                .filter_map(|p| match p {
                    OpenAiContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        None => None,
    }
}

fn image_block_from_url(url: &str) -> ContentBlock {
    if let Some((mime, data)) = parse_data_url(url) {
        ContentBlock::Image(ImageBlock {
            mime,
            source: ImageSource::Base64 { data },
            rendered_by_morph: false,
        })
    } else {
        ContentBlock::Image(ImageBlock {
            mime: guess_mime_from_extension(url),
            source: ImageSource::Url {
                url: url.to_string(),
            },
            rendered_by_morph: false,
        })
    }
}

/// Maps a canonical `StopReason` to the `finish_reason` string OpenAI's
/// Chat Completions API uses on both streaming and buffered responses.
fn finish_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "stop",
        StopReason::MaxTokens => "length",
        StopReason::StopSequence => "stop",
        StopReason::ToolUse => "tool_calls",
        StopReason::Error => "stop",
    }
}

fn error_type(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::InvalidRequest(_) | GatewayError::Unsupported(_) => "invalid_request_error",
        GatewayError::Unauthorized(_) => "authentication_error",
        GatewayError::RateLimited { .. } => "rate_limit_error",
        _ => "api_error",
    }
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- trait impl ---------------------------------------------------------

/// # Streaming framing contract
///
/// `encode_stream_event` emits exactly one `chat.completion.chunk` SSE frame
/// (`data: {...}\n\n`) per call, so it can never itself emit the terminating
/// `data: [DONE]\n\n` sentinel real OpenAI streams end with — there is no
/// `ResponseEvent` that corresponds to "the stream is now closed" separately
/// from `MessageStop`. On `ResponseEvent::MessageStop` this method returns
/// the chunk carrying `finish_reason`; **the caller (morph-gateway) is
/// responsible for appending the literal `data: [DONE]\n\n` line once the
/// underlying event stream itself ends.**
///
/// # Stateless `id`/`model` limitation
///
/// This method takes `&self` and no per-stream context, and the same
/// adapter instance concurrently serves many independent streams, so it
/// cannot remember the `id`/`model` a `MessageStart` event carried when a
/// later `TextDelta`/`ToolCall*`/`Usage`/`MessageStop` event for the *same*
/// stream comes through. Those later chunks emit empty strings for `id` and
/// `model`; a caller that needs wire-perfect fidelity must patch them back
/// in using the values from that stream's `MessageStart` chunk.
impl ProtocolAdapter for OpenAiChatProtocol {
    fn protocol_id(&self) -> &str {
        "openai_chat"
    }

    fn parse_request(&self, raw: &[u8], request_id: &str) -> Result<CanonicalRequest> {
        let wire: OpenAiChatRequest = serde_json::from_slice(raw).map_err(|e| {
            GatewayError::InvalidRequest(format!("invalid openai chat completions body: {e}"))
        })?;

        let mut system_parts = Vec::new();
        let mut messages = Vec::new();

        for m in wire.messages {
            let role_lower = m.role.to_ascii_lowercase();

            if role_lower == "system" || role_lower == "developer" {
                if let Some(text) = content_to_plain_text(&m.content) {
                    if !text.is_empty() {
                        system_parts.push(text);
                    }
                }
                continue;
            }

            let role = match role_lower.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                other => {
                    return Err(GatewayError::InvalidRequest(format!(
                        "unsupported openai message role '{other}'"
                    )))
                }
            };

            let mut blocks = Vec::new();

            if role == Role::Tool {
                let tool_use_id = m.tool_call_id.clone().ok_or_else(|| {
                    GatewayError::InvalidRequest("tool message missing tool_call_id".to_string())
                })?;
                let content = content_to_plain_text(&m.content).unwrap_or_default();
                blocks.push(ContentBlock::ToolResult(ToolResultBlock {
                    tool_use_id,
                    content,
                    is_error: false,
                }));
            } else {
                match &m.content {
                    Some(OpenAiContent::Text(t)) => {
                        if !t.is_empty() {
                            blocks.push(ContentBlock::text(t.clone()));
                        }
                    }
                    Some(OpenAiContent::Parts(parts)) => {
                        for part in parts {
                            match part {
                                OpenAiContentPart::Text { text } => {
                                    blocks.push(ContentBlock::text(text.clone()))
                                }
                                OpenAiContentPart::ImageUrl { image_url } => {
                                    blocks.push(image_block_from_url(&image_url.url))
                                }
                                OpenAiContentPart::Unknown => {}
                            }
                        }
                    }
                    None => {}
                }

                if let Some(tool_calls) = &m.tool_calls {
                    for tc in tool_calls {
                        let input = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|_| Value::String(tc.function.arguments.clone()));
                        blocks.push(ContentBlock::ToolUse(ToolUseBlock {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            input,
                        }));
                    }
                }
            }

            if blocks.is_empty() {
                blocks.push(ContentBlock::text(""));
            }

            messages.push(Message {
                role,
                content: blocks,
                name: m.name.clone(),
            });
        }

        let mut tools = Vec::with_capacity(wire.tools.len());
        for t in wire.tools {
            if t.kind != "function" {
                return Err(GatewayError::Unsupported(format!(
                    "unsupported openai tool type '{}'",
                    t.kind
                )));
            }
            tools.push(ToolDefinition {
                name: t.function.name,
                description: t.function.description,
                parameters: t.function.parameters,
            });
        }

        let tool_choice = wire.tool_choice.map(|tc| match tc {
            OpenAiToolChoice::Mode(mode) => match mode.as_str() {
                "auto" => ToolChoice::Auto,
                "none" => ToolChoice::None,
                "required" => ToolChoice::Required,
                other => ToolChoice::Specific {
                    name: other.to_string(),
                },
            },
            OpenAiToolChoice::Specific { function } => ToolChoice::Specific {
                name: function.name,
            },
        });

        let stop = match wire.stop {
            Some(OpenAiStop::One(s)) => vec![s],
            Some(OpenAiStop::Many(v)) => v,
            None => Vec::new(),
        };

        let response_format = wire.response_format.map(|rf| ResponseFormat {
            kind: rf.kind,
            json_schema: rf.json_schema,
        });

        let extra = if wire.extra.is_empty() {
            Value::Null
        } else {
            Value::Object(wire.extra)
        };

        Ok(CanonicalRequest {
            model: wire.model,
            messages,
            system: if system_parts.is_empty() {
                None
            } else {
                Some(system_parts.join("\n"))
            },
            tools,
            tool_choice,
            temperature: wire.temperature,
            top_p: wire.top_p,
            max_tokens: wire.max_tokens,
            stream: wire.stream,
            stop,
            response_format,
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
        let chunk = match event {
            ResponseEvent::MessageStart { id, model } => json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant", "content": ""},
                    "finish_reason": null,
                }],
            }),
            ResponseEvent::TextDelta { index, text } => json!({
                "id": "",
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": "",
                "choices": [{
                    "index": index,
                    "delta": {"content": text},
                    "finish_reason": null,
                }],
            }),
            ResponseEvent::ToolCallStart { index, id, name } => json!({
                "id": "",
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": "",
                "choices": [{
                    "index": index,
                    "delta": {"tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""},
                    }]},
                    "finish_reason": null,
                }],
            }),
            ResponseEvent::ToolCallDelta {
                index,
                partial_json,
            } => json!({
                "id": "",
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": "",
                "choices": [{
                    "index": index,
                    "delta": {"tool_calls": [{
                        "index": index,
                        "function": {"arguments": partial_json},
                    }]},
                    "finish_reason": null,
                }],
            }),
            // OpenAI has no wire event for "a tool call's arguments are
            // now complete" — the client infers that from `finish_reason`
            // on the terminal chunk instead.
            ResponseEvent::ToolCallEnd { .. } => return Ok(None),
            ResponseEvent::Usage(usage) => json!({
                "id": "",
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": "",
                "choices": [],
                "usage": {
                    "prompt_tokens": usage.input_tokens,
                    "completion_tokens": usage.output_tokens,
                    "total_tokens": usage.input_tokens + usage.output_tokens,
                },
            }),
            ResponseEvent::MessageStop { stop_reason } => json!({
                "id": "",
                "object": "chat.completion.chunk",
                "created": unix_seconds_now(),
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": finish_reason(*stop_reason),
                }],
            }),
        };

        let mut bytes = b"data: ".to_vec();
        bytes.extend_from_slice(&serde_json::to_vec(&chunk)?);
        bytes.extend_from_slice(b"\n\n");
        Ok(Some(bytes))
    }

    fn encode_buffered(&self, resp: &CanonicalResponse) -> Result<Vec<u8>> {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        for block in &resp.content {
            match block {
                ContentBlock::Text(t) => text_parts.push(t.text.clone()),
                ContentBlock::ToolUse(tu) => tool_calls.push(json!({
                    "id": tu.id,
                    "type": "function",
                    "function": {
                        "name": tu.name,
                        "arguments": serde_json::to_string(&tu.input)?,
                    },
                })),
                ContentBlock::Image(_) | ContentBlock::ToolResult(_) => {}
            }
        }

        let content_value = if text_parts.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(text_parts.join(""))
        };

        let mut message = Map::new();
        message.insert("role".to_string(), Value::String("assistant".to_string()));
        message.insert("content".to_string(), content_value);
        if !tool_calls.is_empty() {
            message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }

        let body = json!({
            "id": resp.id,
            "object": "chat.completion",
            "created": unix_seconds_now(),
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": finish_reason(resp.stop_reason),
            }],
            "usage": {
                "prompt_tokens": resp.usage.input_tokens,
                "completion_tokens": resp.usage.output_tokens,
                "total_tokens": resp.usage.input_tokens + resp.usage.output_tokens,
            },
        });

        Ok(serde_json::to_vec(&body)?)
    }

    fn encode_error(&self, err: &GatewayError) -> Vec<u8> {
        let body = json!({
            "error": {
                "message": err.to_string(),
                "type": error_type(err),
                "code": err.status_code(),
            }
        });
        serde_json::to_vec(&body).unwrap_or_else(|_| {
            b"{\"error\":{\"message\":\"internal error\",\"type\":\"api_error\"}}".to_vec()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> OpenAiChatProtocol {
        OpenAiChatProtocol::new()
    }

    #[test]
    fn parses_request_with_tools_and_system_message() {
        let raw = br#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are concise."},
                {"role": "user", "content": "What's the weather in Paris?"}
            ],
            "tools": [
                {"type": "function", "function": {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "parameters": {"type": "object", "properties": {"location": {"type": "string"}}, "required": ["location"]}
                }}
            ],
            "tool_choice": "auto",
            "temperature": 0.2,
            "stream": false,
            "stop": ["\n\n"]
        }"#;

        let req = protocol().parse_request(raw, "req-1").unwrap();

        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.system, Some("You are concise.".to_string()));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(
            req.messages[0].text_content(),
            "What's the weather in Paris?"
        );
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "get_weather");
        assert!(matches!(req.tool_choice, Some(ToolChoice::Auto)));
        assert_eq!(req.temperature, Some(0.2));
        assert_eq!(req.stop, vec!["\n\n".to_string()]);
        assert!(!req.stream);
        assert_eq!(req.metadata.request_id, "req-1");
        assert_eq!(req.metadata.ingress_protocol, "openai_chat");
    }

    #[test]
    fn parses_request_with_image_content_part() {
        let raw = br#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,aVZCT1J3MEtHZ28="}}
                ]}
            ],
            "stream": true
        }"#;

        let req = protocol().parse_request(raw, "req-2").unwrap();

        assert!(req.stream);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content.len(), 2);
        match &req.messages[0].content[1] {
            ContentBlock::Image(img) => {
                assert_eq!(img.mime, "image/png");
                match &img.source {
                    ImageSource::Base64 { data } => assert_eq!(data, "aVZCT1J3MEtHZ28="),
                    ImageSource::Url { .. } => panic!("expected base64 source"),
                }
            }
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn parses_remote_image_url_with_guessed_mime() {
        let raw = br#"{
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "https://example.com/cat.jpeg"}}
                ]}
            ]
        }"#;

        let req = protocol().parse_request(raw, "req-3").unwrap();
        match &req.messages[0].content[0] {
            ContentBlock::Image(img) => {
                assert_eq!(img.mime, "image/jpeg");
                assert!(
                    matches!(&img.source, ImageSource::Url { url } if url == "https://example.com/cat.jpeg")
                );
            }
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn unmapped_fields_are_preserved_in_extra() {
        let raw = br#"{
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "seed": 42,
            "presence_penalty": 0.5
        }"#;

        let req = protocol().parse_request(raw, "req-4").unwrap();
        assert_eq!(req.extra["seed"], json!(42));
        assert_eq!(req.extra["presence_penalty"], json!(0.5));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = protocol().parse_request(b"not json", "req-5").unwrap_err();
        assert!(matches!(err, GatewayError::InvalidRequest(_)));
    }

    #[test]
    fn encodes_text_delta_stream_chunk() {
        let event = ResponseEvent::TextDelta {
            index: 0,
            text: "Hello".to_string(),
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("data: "));
        assert!(s.ends_with("\n\n"));
        let json_part = s.trim_start_matches("data: ").trim_end();
        let parsed: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(parsed["object"], "chat.completion.chunk");
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");
    }

    #[test]
    fn encodes_message_stop_stream_chunk_with_finish_reason() {
        let event = ResponseEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        let s = String::from_utf8(bytes).unwrap();
        let json_part = s.trim_start_matches("data: ").trim_end();
        let parsed: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(parsed["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn tool_call_end_has_no_wire_representation() {
        let event = ResponseEvent::ToolCallEnd { index: 0 };
        assert!(protocol().encode_stream_event(&event).unwrap().is_none());
    }

    #[test]
    fn encodes_buffered_response() {
        let resp = CanonicalResponse {
            id: "resp-1".to_string(),
            model: "gpt-4o".to_string(),
            content: vec![ContentBlock::text("Hi there")],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        let bytes = protocol().encode_buffered(&resp).unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["object"], "chat.completion");
        assert_eq!(parsed["choices"][0]["message"]["content"], "Hi there");
        assert_eq!(parsed["choices"][0]["finish_reason"], "stop");
        assert_eq!(parsed["usage"]["total_tokens"], 15);
    }

    #[test]
    fn encodes_error() {
        let err = GatewayError::InvalidRequest("missing model".to_string());
        let bytes = protocol().encode_error(&err);
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["error"]["type"], "invalid_request_error");
        assert_eq!(parsed["error"]["code"], 400);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing model"));
    }
}
