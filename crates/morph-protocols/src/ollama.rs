//! `ProtocolAdapter` for Ollama's chat wire format (`POST /api/chat`).

use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use morph_core::prelude::*;

use crate::util::{rfc3339_now, sniff_base64_image_mime};

/// Translates Ollama `/api/chat` requests/responses to and from
/// `morph-core`'s canonical types.
///
/// Ollama's request shape has no native tool-calling or structured-output
/// passthrough (unlike OpenAI's `tools`/`response_format`): a `tools` or
/// `format` field sent by the client has no canonical home, so it is kept
/// verbatim in `CanonicalRequest::extra` rather than being parsed or
/// dropped. See [`AnthropicMessagesProtocol`]/[`OpenAiChatProtocol`] for
/// contrast where those fields *do* have first-class canonical fields.
///
/// [`AnthropicMessagesProtocol`]: crate::AnthropicMessagesProtocol
/// [`OpenAiChatProtocol`]: crate::OpenAiChatProtocol
#[derive(Debug, Default)]
pub struct OllamaProtocol;

impl OllamaProtocol {
    pub fn new() -> Self {
        Self
    }
}

// ---- wire-shape DTOs -------------------------------------------------

#[derive(Debug, Deserialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    /// Ollama defaults `stream` to `true` when omitted, unlike OpenAI and
    /// Anthropic which default to `false`.
    #[serde(default = "default_stream_true")]
    stream: bool,
    #[serde(default)]
    options: Option<OllamaOptions>,
    /// `tools`, `format`, `keep_alive`, and anything else this struct
    /// doesn't name land here so they aren't silently dropped.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn default_stream_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    images: Vec<String>,
    /// Per-message fields with no canonical equivalent (e.g. an
    /// experimental `tool_calls` array on an assistant message). `Message`
    /// has no extra-fields slot of its own, so these are surfaced on the
    /// request-level `extra` map instead of being dropped; see
    /// `OllamaProtocol::parse_request`.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct OllamaOptions {
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    num_predict: Option<i64>,
    #[serde(default)]
    stop: Option<OllamaStop>,
    /// `top_k`, `repeat_penalty`, `seed`, ... — no canonical field, kept
    /// verbatim under `extra.options`.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OllamaStop {
    One(String),
    Many(Vec<String>),
}

// ---- helpers -----------------------------------------------------------

/// Maps a canonical `StopReason` to the `done_reason` string Ollama's
/// `/api/chat` responses use.
fn ollama_done_reason(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "stop",
        StopReason::MaxTokens => "length",
        StopReason::StopSequence => "stop",
        // Ollama's `/api/chat` has no dedicated done_reason for tool calls.
        StopReason::ToolUse => "stop",
        StopReason::Error => "error",
    }
}

// ---- trait impl ---------------------------------------------------------

impl ProtocolAdapter for OllamaProtocol {
    fn protocol_id(&self) -> &str {
        "ollama"
    }

    fn parse_request(&self, raw: &[u8], request_id: &str) -> Result<CanonicalRequest> {
        let wire: OllamaRequest = serde_json::from_slice(raw)
            .map_err(|e| GatewayError::InvalidRequest(format!("invalid ollama chat body: {e}")))?;

        let mut system_parts = Vec::new();
        let mut messages = Vec::new();
        let mut message_extras = Vec::new();

        for (idx, m) in wire.messages.into_iter().enumerate() {
            if !m.extra.is_empty() {
                message_extras
                    .push(json!({"index": idx, "fields": Value::Object(m.extra.clone())}));
            }

            let role_lower = m.role.to_ascii_lowercase();
            if role_lower == "system" {
                if !m.content.is_empty() {
                    system_parts.push(m.content.clone());
                }
                continue;
            }

            let role = match role_lower.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                other => {
                    return Err(GatewayError::InvalidRequest(format!(
                        "unsupported ollama message role '{other}'"
                    )))
                }
            };

            let mut blocks = Vec::new();
            if !m.content.is_empty() {
                blocks.push(ContentBlock::text(m.content.clone()));
            }
            for image in &m.images {
                blocks.push(ContentBlock::Image(ImageBlock {
                    mime: sniff_base64_image_mime(image),
                    source: ImageSource::Base64 {
                        data: image.clone(),
                    },
                    rendered_by_morph: false,
                }));
            }
            if blocks.is_empty() {
                blocks.push(ContentBlock::text(""));
            }

            messages.push(Message {
                role,
                content: blocks,
                name: None,
            });
        }

        let (temperature, top_p, max_tokens, stop, options_extra) = match wire.options {
            Some(opts) => {
                // -1/-2 mean "unlimited"/"use num_ctx" in Ollama and have no
                // representable positive `u32` canonical equivalent.
                let max_tokens = opts.num_predict.filter(|n| *n > 0).map(|n| n as u32);
                let stop = match opts.stop {
                    Some(OllamaStop::One(s)) => vec![s],
                    Some(OllamaStop::Many(v)) => v,
                    None => Vec::new(),
                };
                (opts.temperature, opts.top_p, max_tokens, stop, opts.extra)
            }
            None => (None, None, None, Vec::new(), Map::new()),
        };

        let mut extra_map = wire.extra;
        if !options_extra.is_empty() {
            extra_map.insert("options".to_string(), Value::Object(options_extra));
        }
        if !message_extras.is_empty() {
            extra_map.insert(
                "_ollama_message_extra".to_string(),
                Value::Array(message_extras),
            );
        }
        let extra = if extra_map.is_empty() {
            Value::Null
        } else {
            Value::Object(extra_map)
        };

        Ok(CanonicalRequest {
            model: wire.model,
            messages,
            system: if system_parts.is_empty() {
                None
            } else {
                Some(system_parts.join("\n"))
            },
            tools: Vec::new(),
            tool_choice: None,
            temperature,
            top_p,
            max_tokens,
            stream: wire.stream,
            stop,
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

    /// Ollama's `/api/chat` streaming shape is NDJSON, not SSE: one bare
    /// JSON object per line (no `data: ` prefix), with a single terminal
    /// line carrying `"done": true`.
    ///
    /// `MessageStart` and every `ToolCall*` variant return `Ok(None)`:
    /// Ollama has no "stream started" line, and `/api/chat` streaming
    /// never emits incremental tool-call deltas (a tool call only ever
    /// appears whole, on the final message). `Usage` also returns
    /// `Ok(None)` — this is the exact case the trait's own doc comment
    /// calls out ("a bare `Usage` event for a protocol that only reports
    /// usage at stream end"): Ollama only ever reports `eval_count`/
    /// `prompt_eval_count` bundled onto the `done: true` line, which this
    /// adapter emits from `MessageStop`.
    ///
    /// Because this method is stateless (`&self`, no per-stream context,
    /// and one adapter instance serves many concurrent streams), the
    /// `MessageStop` line cannot recover the numeric values carried by an
    /// earlier, separately-delivered `Usage` event, so `eval_count`/
    /// `prompt_eval_count` are emitted as `0` here. A gateway that wants
    /// exact fidelity should special-case Ollama: remember the last `Usage`
    /// event per stream and patch these two fields into the `done: true`
    /// line before writing it to the wire. `encode_buffered` below has no
    /// such limitation, since `CanonicalResponse::usage` is populated
    /// directly.
    fn encode_stream_event(&self, event: &ResponseEvent) -> Result<Option<Vec<u8>>> {
        let line = match event {
            ResponseEvent::MessageStart { .. } => return Ok(None),
            ResponseEvent::TextDelta { text, .. } => json!({
                "model": "",
                "created_at": rfc3339_now(),
                "message": {"role": "assistant", "content": text},
                "done": false,
            }),
            ResponseEvent::ToolCallStart { .. }
            | ResponseEvent::ToolCallDelta { .. }
            | ResponseEvent::ToolCallEnd { .. } => return Ok(None),
            ResponseEvent::Usage(_) => return Ok(None),
            ResponseEvent::MessageStop { stop_reason } => json!({
                "model": "",
                "created_at": rfc3339_now(),
                "message": {"role": "assistant", "content": ""},
                "done": true,
                "done_reason": ollama_done_reason(*stop_reason),
                "eval_count": 0,
                "prompt_eval_count": 0,
            }),
        };

        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        Ok(Some(bytes))
    }

    fn encode_buffered(&self, resp: &CanonicalResponse) -> Result<Vec<u8>> {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        for block in &resp.content {
            match block {
                ContentBlock::Text(t) => text_parts.push(t.text.clone()),
                ContentBlock::ToolUse(tu) => {
                    tool_calls.push(json!({"function": {"name": tu.name, "arguments": tu.input}}))
                }
                ContentBlock::Image(_) | ContentBlock::ToolResult(_) => {}
            }
        }

        let mut message = Map::new();
        message.insert("role".to_string(), Value::String("assistant".to_string()));
        message.insert("content".to_string(), Value::String(text_parts.join("")));
        if !tool_calls.is_empty() {
            message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }

        let body = json!({
            "model": resp.model,
            "created_at": rfc3339_now(),
            "message": Value::Object(message),
            "done": true,
            "done_reason": ollama_done_reason(resp.stop_reason),
            "eval_count": resp.usage.output_tokens,
            "prompt_eval_count": resp.usage.input_tokens,
        });

        Ok(serde_json::to_vec(&body)?)
    }

    fn encode_error(&self, err: &GatewayError) -> Vec<u8> {
        let body = json!({"error": err.to_string()});
        serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"error\":\"internal error\"}".to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> OllamaProtocol {
        OllamaProtocol::new()
    }

    #[test]
    fn parses_request_with_images() {
        let raw = br#"{
            "model": "llava",
            "messages": [
                {"role": "user", "content": "What's in this image?", "images": ["iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"]}
            ],
            "stream": false
        }"#;

        let req = protocol().parse_request(raw, "req-1").unwrap();

        assert!(!req.stream);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content.len(), 2);
        assert_eq!(
            req.messages[0].content[0].as_text(),
            Some("What's in this image?")
        );
        match &req.messages[0].content[1] {
            ContentBlock::Image(img) => {
                assert_eq!(img.mime, "image/png");
                assert!(
                    matches!(&img.source, ImageSource::Base64 { data } if data == "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB")
                );
            }
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn stream_defaults_to_true_when_omitted() {
        let raw = br#"{
            "model": "llama3.1",
            "messages": [{"role": "user", "content": "hi"}]
        }"#;
        let req = protocol().parse_request(raw, "req-2").unwrap();
        assert!(req.stream);
    }

    #[test]
    fn unmapped_top_level_tools_field_is_preserved_in_extra() {
        let raw = br#"{
            "model": "llama3.1",
            "messages": [{"role": "user", "content": "Get me the weather in Paris"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}],
            "stream": true
        }"#;

        let req = protocol().parse_request(raw, "req-3").unwrap();

        // Ollama's /api/chat has no canonical tool-calling home, so this
        // must not be silently mapped or dropped.
        assert!(req.tools.is_empty());
        assert_eq!(req.extra["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn options_map_to_canonical_fields_and_preserve_the_rest() {
        let raw = br#"{
            "model": "llama3.1",
            "messages": [{"role": "system", "content": "Be brief."}, {"role": "user", "content": "hi"}],
            "options": {"temperature": 0.4, "top_p": 0.9, "num_predict": 128, "top_k": 40, "stop": ["\n"]}
        }"#;

        let req = protocol().parse_request(raw, "req-4").unwrap();

        assert_eq!(req.system, Some("Be brief.".to_string()));
        assert_eq!(req.temperature, Some(0.4));
        assert_eq!(req.top_p, Some(0.9));
        assert_eq!(req.max_tokens, Some(128));
        assert_eq!(req.stop, vec!["\n".to_string()]);
        assert_eq!(req.extra["options"]["top_k"], 40);
    }

    #[test]
    fn rejects_malformed_json() {
        let err = protocol().parse_request(b"not json", "req-5").unwrap_err();
        assert!(matches!(err, GatewayError::InvalidRequest(_)));
    }

    #[test]
    fn encodes_text_delta_as_ndjson_line() {
        let event = ResponseEvent::TextDelta {
            index: 0,
            text: "Hello".to_string(),
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        assert!(!bytes.starts_with(b"data: "));
        assert_eq!(*bytes.last().unwrap(), b'\n');
        let line = std::str::from_utf8(&bytes).unwrap().trim_end();
        let parsed: Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["message"]["content"], "Hello");
        assert_eq!(parsed["done"], false);
    }

    #[test]
    fn encodes_message_stop_as_final_done_line() {
        let event = ResponseEvent::MessageStop {
            stop_reason: StopReason::MaxTokens,
        };
        let bytes = protocol().encode_stream_event(&event).unwrap().unwrap();
        let line = std::str::from_utf8(&bytes).unwrap().trim_end();
        let parsed: Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["done"], true);
        assert_eq!(parsed["done_reason"], "length");
    }

    #[test]
    fn usage_and_message_start_have_no_wire_representation() {
        assert!(protocol()
            .encode_stream_event(&ResponseEvent::Usage(Usage {
                input_tokens: 1,
                output_tokens: 2,
            }))
            .unwrap()
            .is_none());
        assert!(protocol()
            .encode_stream_event(&ResponseEvent::MessageStart {
                id: "1".to_string(),
                model: "llama3.1".to_string(),
            })
            .unwrap()
            .is_none());
    }

    #[test]
    fn encodes_buffered_response_with_real_usage_counts() {
        let resp = CanonicalResponse {
            id: "ignored-by-ollama".to_string(),
            model: "llama3.1".to_string(),
            content: vec![ContentBlock::text("Hi there")],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
        };
        let bytes = protocol().encode_buffered(&resp).unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["message"]["content"], "Hi there");
        assert_eq!(parsed["done"], true);
        assert_eq!(parsed["eval_count"], 5);
        assert_eq!(parsed["prompt_eval_count"], 10);
    }

    #[test]
    fn encodes_error() {
        let err = GatewayError::Internal("boom".to_string());
        let bytes = protocol().encode_error(&err);
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["error"], "internal error: boom");
    }
}
