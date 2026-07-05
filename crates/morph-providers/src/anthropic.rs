//! Provider adapter for the Anthropic Messages API wire format.
//!
//! Unlike the OpenAI-wire adapter, Anthropic's Messages API keeps tool use
//! and tool results inline as content blocks on ordinary `user`/`assistant`
//! messages rather than reshaping them into dedicated message roles, so the
//! canonical-to-wire mapping here is considerably more direct.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::Poll;

use async_trait::async_trait;
use eventsource_stream::{Event as SseEvent, EventStreamError, Eventsource};
use futures::Stream;
use serde_json::{json, Map, Value};

use morph_core::capabilities::Capabilities;
use morph_core::error::GatewayError;
use morph_core::message::{ContentBlock, ImageBlock, ImageSource, Role, ToolChoice};
use morph_core::request::{CanonicalRequest, ResponseEvent, StopReason, Usage};
use morph_core::stream::ResponseStream;
use morph_core::traits::ProviderAdapter;

use crate::util::{passthrough_headers, send_request};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens` on every request; this is the fallback
/// used when `CanonicalRequest.max_tokens` is not set.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A `ProviderAdapter` speaking the Anthropic Messages API wire format.
///
/// `base_url` is supplied by the caller (a sensible default is
/// `"https://api.anthropic.com/v1"`, but this adapter doesn't hardcode it so
/// Anthropic-compatible gateways/proxies work too). Authentication uses the
/// `x-api-key` header rather than `Authorization: Bearer`, matching
/// Anthropic's actual API; the header is omitted entirely when `api_key` is
/// `None`, for compatibility with proxies that inject their own auth.
///
/// When `passthrough_auth` is set, `api_key` is ignored entirely and every
/// header from the client's original request to Morph (minus a small
/// deny-list of hop-by-hop/body-describing ones — see
/// `util::passthrough_headers`) is replayed verbatim on the upstream call
/// instead. This is what makes an OAuth-backed claude.ai subscription login
/// work through Morph with no Anthropic API key configured at all: Morph
/// doesn't need to understand *which* header carries the credential, it
/// just forwards whatever the client already sent.
pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    passthrough_auth: bool,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
        passthrough_auth: bool,
    ) -> Self {
        AnthropicProvider {
            name: name.into(),
            base_url: base_url.into(),
            api_key,
            passthrough_auth,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tools: true,
            vision: true,
            // Anthropic's Messages API has no native `response_format`
            // parameter; structured output is achieved by the caller
            // forcing a single tool use (`tool_choice: {"type": "tool", ...}`)
            // and treating that tool's input as the structured payload.
            // Translating a canonical `response_format` into that pattern is
            // the pipeline/protocol layer's job, not this adapter's.
            structured_output: true,
            reasoning: true,
            max_context_tokens: None,
        }
    }

    async fn send(
        &self,
        req: CanonicalRequest,
        incoming_headers: &[(String, String)],
    ) -> Result<ResponseStream, GatewayError> {
        let body = build_request_body(&req);
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));

        let mut builder = self.client.post(url).json(&body);
        if self.passthrough_auth {
            for (name, value) in passthrough_headers(incoming_headers) {
                builder = builder.header(name, value);
            }
            // Only fill in a default if the client didn't already send its
            // own (real Anthropic clients, including Claude Code, always do).
            if !incoming_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
            {
                builder = builder.header("anthropic-version", ANTHROPIC_VERSION);
            }
        } else {
            builder = builder.header("anthropic-version", ANTHROPIC_VERSION);
            if let Some(key) = &self.api_key {
                builder = builder.header("x-api-key", key);
            }
        }

        let response = send_request(builder).await?;
        let sse: Pin<
            Box<dyn Stream<Item = Result<SseEvent, EventStreamError<reqwest::Error>>> + Send>,
        > = Box::pin(response.bytes_stream().eventsource());

        let stream: ResponseStream = Box::pin(stream_events(sse));
        Ok(stream)
    }
}

fn stream_events(
    mut sse: Pin<Box<dyn Stream<Item = Result<SseEvent, EventStreamError<reqwest::Error>>> + Send>>,
) -> impl Stream<Item = Result<ResponseEvent, GatewayError>> + Send + 'static {
    let mut state = StreamState::default();
    let mut pending: VecDeque<ResponseEvent> = VecDeque::new();

    futures::stream::poll_fn(move |cx| loop {
        if let Some(event) = pending.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }

        match sse.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(sse_event))) => {
                let data = sse_event.data.trim();
                if data.is_empty() {
                    continue;
                }
                match apply_event(data, &mut state, &mut pending) {
                    Ok(()) => continue,
                    Err(err) => return Poll::Ready(Some(Err(err))),
                }
            }
            Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(map_sse_error(err)))),
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
        }
    })
}

/// Per-stream state: the stop reason reported by `message_delta` (Anthropic
/// splits "what the stop reason is" from "the stream is now over" across two
/// separate SSE events), and which content block indices are currently open
/// `tool_use` blocks so `content_block_stop` can be translated into
/// `ToolCallEnd` only for those, not for ordinary text blocks.
#[derive(Default)]
struct StreamState {
    pending_stop_reason: Option<StopReason>,
    open_tool_use_indices: std::collections::BTreeSet<usize>,
}

/// Maps one upstream Anthropic SSE event to zero or more `ResponseEvent`s.
/// Dispatch is driven by the JSON payload's own `"type"` field rather than
/// the SSE `event:` line, since Anthropic always sets both to the same
/// value and matching on the parsed payload is one fewer thing to keep in
/// sync.
fn apply_event(
    data: &str,
    state: &mut StreamState,
    out: &mut VecDeque<ResponseEvent>,
) -> Result<(), GatewayError> {
    let value: Value = serde_json::from_str(data)?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match event_type {
        "message_start" => {
            let message = value.get("message");
            let id = message
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let model = message
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            out.push_back(ResponseEvent::MessageStart { id, model });
            if let Some(usage) = message.and_then(|m| m.get("usage")) {
                out.push_back(ResponseEvent::Usage(parse_usage(usage)));
            }
        }
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(block) = value.get("content_block") {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    state.open_tool_use_indices.insert(index);
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    out.push_back(ResponseEvent::ToolCallStart { index, id, name });
                }
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(delta) = value.get("delta") {
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !text.is_empty() {
                            out.push_back(ResponseEvent::TextDelta { index, text });
                        }
                    }
                    Some("input_json_delta") => {
                        let partial_json = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if !partial_json.is_empty() {
                            out.push_back(ResponseEvent::ToolCallDelta {
                                index,
                                partial_json,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if state.open_tool_use_indices.remove(&index) {
                out.push_back(ResponseEvent::ToolCallEnd { index });
            }
        }
        "message_delta" => {
            if let Some(reason) = value
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                state.pending_stop_reason = Some(map_stop_reason(reason));
            }
            if let Some(usage) = value.get("usage") {
                out.push_back(ResponseEvent::Usage(parse_usage(usage)));
            }
        }
        "message_stop" => {
            let stop_reason = state
                .pending_stop_reason
                .take()
                .unwrap_or(StopReason::EndTurn);
            out.push_back(ResponseEvent::MessageStop { stop_reason });
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown upstream error")
                .to_string();
            return Err(GatewayError::Upstream {
                status: None,
                message,
            });
        }
        // "ping" heartbeats and any future event type we don't recognize
        // carry no canonical representation and are silently skipped.
        _ => {}
    }

    Ok(())
}

fn parse_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        _ => StopReason::Error,
    }
}

fn map_sse_error(err: EventStreamError<reqwest::Error>) -> GatewayError {
    match err {
        EventStreamError::Transport(err) => {
            if err.is_timeout() {
                GatewayError::Timeout(0)
            } else {
                GatewayError::Upstream {
                    status: err.status().map(|s| s.as_u16()),
                    message: err.to_string(),
                }
            }
        }
        other => GatewayError::Protocol(format!("malformed upstream SSE stream: {other}")),
    }
}

/// Builds the JSON body sent to `{base_url}/messages`. Streaming is always
/// requested upstream (`"stream": true`); see the equivalent note on the
/// OpenAI adapter for why.
fn build_request_body(req: &CanonicalRequest) -> Value {
    let mut messages = Vec::new();
    let mut system_parts: Vec<String> = Vec::new();

    if let Some(system) = &req.system {
        if !system.is_empty() {
            system_parts.push(system.clone());
        }
    }

    for message in &req.messages {
        match message.role {
            // Anthropic's Messages API has no "system" message role: fold
            // any canonical system-role message into the top-level "system"
            // field instead of dropping it.
            Role::System => {
                let text = message.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            // Anthropic has no "tool" role either — tool results are just
            // `tool_result` content blocks inside a "user" message.
            Role::User | Role::Tool => {
                if let Some(content) = map_content_blocks(&message.content) {
                    messages.push(json!({"role": "user", "content": content}));
                }
            }
            Role::Assistant => {
                if let Some(content) = map_content_blocks(&message.content) {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert(
        "max_tokens".into(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    if !system_parts.is_empty() {
        body.insert("system".into(), json!(system_parts.join("\n\n")));
    }
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                let mut obj = Map::new();
                obj.insert("name".into(), json!(tool.name));
                if let Some(description) = &tool.description {
                    obj.insert("description".into(), json!(description));
                }
                obj.insert("input_schema".into(), tool.parameters.clone());
                Value::Object(obj)
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), map_tool_choice(choice));
    }
    if let Some(temperature) = req.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if !req.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(req.stop));
    }

    Value::Object(body)
}

fn map_content_blocks(content: &[ContentBlock]) -> Option<Value> {
    if content.is_empty() {
        return None;
    }
    let blocks: Vec<Value> = content.iter().map(map_content_block).collect();
    Some(Value::Array(blocks))
}

fn map_content_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(text) => json!({"type": "text", "text": text.text}),
        ContentBlock::Image(image) => json!({"type": "image", "source": map_image_source(image)}),
        ContentBlock::ToolUse(tool_use) => json!({
            "type": "tool_use",
            "id": tool_use.id,
            "name": tool_use.name,
            "input": tool_use.input,
        }),
        ContentBlock::ToolResult(result) => {
            let mut obj = Map::new();
            obj.insert("type".into(), json!("tool_result"));
            obj.insert("tool_use_id".into(), json!(result.tool_use_id));
            obj.insert("content".into(), json!(result.content));
            if result.is_error {
                obj.insert("is_error".into(), json!(true));
            }
            Value::Object(obj)
        }
    }
}

fn map_image_source(image: &ImageBlock) -> Value {
    match &image.source {
        ImageSource::Base64 { data } => json!({
            "type": "base64",
            "media_type": image.mime,
            "data": data,
        }),
        ImageSource::Url { url } => json!({"type": "url", "url": url}),
    }
}

fn map_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"type": "auto"}),
        // Anthropic has no wire-level "none": the caller achieves the same
        // effect by omitting `tools` entirely. Emitting `{"type": "none"}`
        // here is best-effort forward compatibility should Anthropic add
        // one; older API versions will reject it, which is no worse than
        // silently ignoring the client's explicit request.
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Required => json!({"type": "any"}),
        ToolChoice::Specific { name } => json!({"type": "tool", "name": name}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use morph_core::message::{Message, ToolDefinition, ToolResultBlock, ToolUseBlock};
    use morph_core::request::RequestMetadata;
    use std::time::SystemTime;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn base_request(model: &str, messages: Vec<Message>) -> CanonicalRequest {
        CanonicalRequest {
            model: model.to_string(),
            messages,
            system: None,
            tools: Vec::new(),
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: true,
            stop: Vec::new(),
            response_format: None,
            reasoning: None,
            metadata: RequestMetadata {
                request_id: "req-1".to_string(),
                ingress_protocol: "test".to_string(),
                received_at: SystemTime::now(),
            },
            extra: Value::Null,
        }
    }

    #[test]
    fn defaults_max_tokens_when_absent() {
        let req = base_request("claude-3-5-sonnet", vec![Message::user("hi")]);
        let body = build_request_body(&req);
        assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn folds_system_message_into_top_level_system_field() {
        let mut req = base_request("claude-3-5-sonnet", vec![Message::user("hi")]);
        req.system = Some("be nice".to_string());
        let body = build_request_body(&req);
        assert_eq!(body["system"], json!("be nice"));
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
    }

    #[test]
    fn folds_inline_system_role_messages_and_never_emits_them_as_messages() {
        let mut req = base_request(
            "claude-3-5-sonnet",
            vec![
                Message {
                    role: Role::System,
                    content: vec![ContentBlock::text("Reminder: stay in character.")],
                    name: None,
                },
                Message::user("hi"),
            ],
        );
        req.system = Some("Be terse.".to_string());

        let body = build_request_body(&req);

        // The top-level system field and the inline system-role message are
        // combined, in encounter order, top-level first.
        assert_eq!(
            body["system"],
            json!("Be terse.\n\nReminder: stay in character.")
        );
        // Only the user turn ends up in `messages` — a `role: "system"`
        // entry there is rejected outright by Anthropic's real API.
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
    }

    #[test]
    fn maps_tool_use_and_tool_result_blocks_inline() {
        let assistant_msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse(ToolUseBlock {
                id: "toolu_1".to_string(),
                name: "get_weather".to_string(),
                input: json!({"city": "Lisbon"}),
            })],
            name: None,
        };
        let tool_result_msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: "toolu_1".to_string(),
                content: "sunny".to_string(),
                is_error: false,
            })],
            name: None,
        };
        let req = base_request("claude-3-5-sonnet", vec![assistant_msg, tool_result_msg]);
        let body = build_request_body(&req);
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], json!("assistant"));
        assert_eq!(messages[0]["content"][0]["type"], json!("tool_use"));
        assert_eq!(messages[0]["content"][0]["id"], json!("toolu_1"));

        assert_eq!(messages[1]["role"], json!("user"));
        assert_eq!(messages[1]["content"][0]["type"], json!("tool_result"));
        assert_eq!(messages[1]["content"][0]["tool_use_id"], json!("toolu_1"));
    }

    #[test]
    fn maps_tools_and_tool_choice() {
        let mut req = base_request("claude-3-5-sonnet", vec![Message::user("hi")]);
        req.tools.push(ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("gets the weather".to_string()),
            parameters: json!({"type": "object", "properties": {}}),
        });
        req.tool_choice = Some(ToolChoice::Specific {
            name: "get_weather".to_string(),
        });
        let body = build_request_body(&req);
        assert_eq!(
            body["tools"][0]["input_schema"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type": "tool", "name": "get_weather"})
        );
    }

    #[tokio::test]
    async fn streams_text_deltas_tool_calls_usage_and_stop_reason() {
        let mock_server = MockServer::start().await;

        let sse_body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-3-5-sonnet\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello, \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"Lisbon\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );

        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse_body.as_bytes(), "text/event-stream"),
            )
            .mount(&mock_server)
            .await;

        let provider = AnthropicProvider::new(
            "anthropic",
            mock_server.uri(),
            Some("sk-ant-test".to_string()),
            false,
        );
        let req = base_request(
            "claude-3-5-sonnet",
            vec![Message::user("what's the weather?")],
        );
        let mut stream = provider.send(req, &[]).await.expect("send should succeed");

        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event should not be an error"));
        }

        assert!(matches!(
            &events[0],
            ResponseEvent::MessageStart { id, model }
                if id == "msg_1" && model == "claude-3-5-sonnet"
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::Usage(usage) if usage.input_tokens == 10 && usage.output_tokens == 0
        )));

        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world!");

        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::ToolCallStart { index, id, name }
                if *index == 1 && id == "toolu_1" && name == "get_weather"
        )));

        let partial_json: String = events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::ToolCallDelta { partial_json, .. } => Some(partial_json.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(partial_json, "{\"city\":\"Lisbon\"}");

        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCallEnd { index } if *index == 1)));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::Usage(usage) if usage.input_tokens == 0 && usage.output_tokens == 15
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::MessageStop { stop_reason } if *stop_reason == StopReason::ToolUse
        )));

        // The text block's `content_block_stop` (index 0) must not produce a
        // `ToolCallEnd` -- only the tool_use block (index 1) should.
        assert!(!events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCallEnd { index } if *index == 0)));
    }

    #[tokio::test]
    async fn non_2xx_upstream_response_maps_to_gateway_error_upstream() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("{\"error\":{\"message\":\"forbidden\"}}"),
            )
            .mount(&mock_server)
            .await;

        let provider = AnthropicProvider::new(
            "anthropic",
            mock_server.uri(),
            Some("sk-ant-test".to_string()),
            false,
        );
        let req = base_request("claude-3-5-sonnet", vec![Message::user("hi")]);
        let err = provider.send(req, &[]).await.err().expect("should fail");

        match err {
            GatewayError::Upstream { status, message } => {
                assert_eq!(status, Some(403));
                assert!(message.contains("forbidden"));
            }
            other => panic!("expected Upstream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn passthrough_auth_forwards_clients_own_headers_and_ignores_configured_key() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/messages"))
            // These exact-match assertions are the whole test: if
            // passthrough_auth forwarded the wrong thing (or the configured
            // `api_key` leaked through instead), wiremock has no matching
            // mock, the mock server 404s, and `send()` below fails.
            .and(header("anthropic-beta", "oauth-2026-01-01"))
            .and(header("authorization", "Bearer client-oauth-token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                concat!(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-3-5-sonnet\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                ),
                "text/event-stream",
            ))
            .mount(&mock_server)
            .await;

        // Configured with an api_key that must be ignored, and
        // passthrough_auth = true.
        let provider = AnthropicProvider::new(
            "anthropic",
            mock_server.uri(),
            Some("configured-key-must-not-be-sent".to_string()),
            true,
        );
        let req = base_request("claude-3-5-sonnet", vec![Message::user("hi")]);
        let incoming_headers = vec![
            (
                "authorization".to_string(),
                "Bearer client-oauth-token".to_string(),
            ),
            ("anthropic-beta".to_string(), "oauth-2026-01-01".to_string()),
            // Must be dropped, not forwarded (would desync from the new body).
            ("content-length".to_string(), "9999".to_string()),
        ];

        let mut stream = provider.send(req, &incoming_headers).await.expect(
            "send should succeed — wiremock only matches if headers were forwarded correctly",
        );
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event should not be an error"));
        }
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::MessageStop { .. })));
    }
}
