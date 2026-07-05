//! Generic provider adapter for the OpenAI Chat Completions wire format.
//!
//! This single adapter is what makes OpenAI, Azure OpenAI, Ollama, vLLM, LM
//! Studio, OpenRouter, Together, Groq, Cerebras, Mistral, DeepSeek, and xAI
//! all work as Morph backends: they all speak this exact request/response
//! shape, differing only in `base_url` and how (or whether) they want an API
//! key. Nothing in here may assume anything OpenAI-specific that wouldn't
//! hold for e.g. a local Ollama server, which typically needs no
//! `Authorization` header at all.

use std::collections::{BTreeSet, VecDeque};
use std::pin::Pin;
use std::task::Poll;

use async_trait::async_trait;
use eventsource_stream::{Event as SseEvent, EventStreamError, Eventsource};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use morph_core::capabilities::Capabilities;
use morph_core::error::GatewayError;
use morph_core::message::{ContentBlock, ImageBlock, ImageSource, Message, Role, ToolChoice};
use morph_core::request::{CanonicalRequest, ResponseEvent, ResponseFormat, StopReason, Usage};
use morph_core::stream::ResponseStream;
use morph_core::traits::ProviderAdapter;

use crate::util::{passthrough_headers, send_request};

/// A `ProviderAdapter` speaking the OpenAI `/chat/completions` wire format.
///
/// `base_url` and `api_key` are supplied by the caller (config), never
/// hardcoded here, so one implementation covers every OpenAI-wire-compatible
/// backend. When `api_key` is `None` no `Authorization` header is sent at
/// all, which is required for backends (local Ollama, some vLLM deployments)
/// that reject requests carrying an unexpected auth header just as readily
/// as ones missing a required one.
///
/// When `passthrough_auth` is set, `api_key` is ignored and every header
/// from the client's original request (minus a small deny-list — see
/// `util::passthrough_headers`) is replayed verbatim on the upstream call
/// instead, so Morph never needs its own credential for this backend.
pub struct OpenAiProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    passthrough_auth: bool,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
        passthrough_auth: bool,
    ) -> Self {
        OpenAiProvider {
            name: name.into(),
            base_url: base_url.into(),
            api_key,
            passthrough_auth,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            tools: true,
            vision: true,
            structured_output: true,
            reasoning: false,
            // Context window varies wildly across the backends this adapter
            // covers (a local 8k Ollama model vs. a 128k+ hosted one) and
            // isn't discoverable from the wire protocol alone, so it's left
            // unknown here; callers that need it should source it from
            // per-model config instead.
            max_context_tokens: None,
        }
    }

    async fn send(
        &self,
        req: CanonicalRequest,
        incoming_headers: &[(String, String)],
    ) -> Result<ResponseStream, GatewayError> {
        let body = build_request_body(&req);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut builder = self.client.post(url).json(&body);
        if self.passthrough_auth {
            for (name, value) in passthrough_headers(incoming_headers) {
                builder = builder.header(name, value);
            }
        } else if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let response = send_request(builder).await?;
        let sse: Pin<
            Box<dyn Stream<Item = Result<SseEvent, EventStreamError<reqwest::Error>>> + Send>,
        > = Box::pin(response.bytes_stream().eventsource());

        let stream: ResponseStream = Box::pin(stream_events(sse));
        Ok(stream)
    }
}

/// Turns the raw upstream SSE event stream into the canonical
/// `ResponseEvent` stream. Kept as a free function (rather than inlined in
/// `send`) so the chunk -> event(s) mapping in [`apply_chunk`] can be
/// exercised without spinning up a mock HTTP server.
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
                // OpenAI terminates the stream with a sentinel `data: [DONE]`
                // event that carries no chunk payload.
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                match apply_chunk(data, &mut state, &mut pending) {
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

/// Per-stream state that must persist across chunks: whether
/// `ResponseEvent::MessageStart` has already been emitted, and which tool
/// call indices are currently "open" (started but not yet ended) so they can
/// be closed out, best-effort, when `finish_reason` arrives.
#[derive(Default)]
struct StreamState {
    started: bool,
    open_tool_calls: BTreeSet<usize>,
}

/// Maps one upstream `chat.completion.chunk` JSON payload to zero or more
/// `ResponseEvent`s, appended to `out` in emission order.
fn apply_chunk(
    data: &str,
    state: &mut StreamState,
    out: &mut VecDeque<ResponseEvent>,
) -> Result<(), GatewayError> {
    let chunk: ChatCompletionChunk = serde_json::from_str(data)?;

    if !state.started {
        state.started = true;
        out.push_back(ResponseEvent::MessageStart {
            id: chunk.id.unwrap_or_default(),
            model: chunk.model.unwrap_or_default(),
        });
    }

    for choice in &chunk.choices {
        if let Some(text) = &choice.delta.content {
            if !text.is_empty() {
                out.push_back(ResponseEvent::TextDelta {
                    index: choice.index,
                    text: text.clone(),
                });
            }
        }

        for tool_call in &choice.delta.tool_calls {
            let index = tool_call.index;
            // `BTreeSet::insert` returns `true` only the first time an index
            // is seen, which is exactly "first appearance" per index.
            if state.open_tool_calls.insert(index) {
                out.push_back(ResponseEvent::ToolCallStart {
                    index,
                    id: tool_call.id.clone().unwrap_or_default(),
                    name: tool_call
                        .function
                        .as_ref()
                        .and_then(|f| f.name.clone())
                        .unwrap_or_default(),
                });
            }
            if let Some(args) = tool_call
                .function
                .as_ref()
                .and_then(|f| f.arguments.clone())
            {
                if !args.is_empty() {
                    out.push_back(ResponseEvent::ToolCallDelta {
                        index,
                        partial_json: args,
                    });
                }
            }
        }

        if let Some(reason) = &choice.finish_reason {
            // Best-effort: OpenAI's wire format has no explicit "tool call
            // ended" signal, so every still-open tool call is closed the
            // moment a finish reason arrives.
            for index in std::mem::take(&mut state.open_tool_calls) {
                out.push_back(ResponseEvent::ToolCallEnd { index });
            }
            out.push_back(ResponseEvent::MessageStop {
                stop_reason: map_finish_reason(reason),
            });
        }
    }

    if let Some(usage) = chunk.usage {
        out.push_back(ResponseEvent::Usage(Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        }));
    }

    Ok(())
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        // "function_call" is the deprecated predecessor of "tool_calls".
        "tool_calls" | "function_call" => StopReason::ToolUse,
        // "content_filter" and any backend-specific reason we don't
        // recognize are surfaced as a generic error stop rather than
        // silently mislabeled as a normal completion.
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

#[derive(Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChunkToolCall>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkFunction>,
}

#[derive(Deserialize)]
struct ChunkFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

/// Builds the JSON body sent to `{base_url}/chat/completions`. Streaming is
/// always requested upstream (`"stream": true`) regardless of what the
/// original client asked for — the protocol layer, not this adapter, decides
/// whether to re-buffer the resulting event stream for a non-streaming
/// client. `stream_options.include_usage` is likewise always set so that
/// backends which support it (OpenAI itself, and most OpenAI-wire-compatible
/// ones) report token usage on the final chunk.
fn build_request_body(req: &CanonicalRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    for message in &req.messages {
        append_openai_messages(message, &mut messages);
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                let mut function = Map::new();
                function.insert("name".into(), json!(tool.name));
                if let Some(description) = &tool.description {
                    function.insert("description".into(), json!(description));
                }
                function.insert("parameters".into(), tool.parameters.clone());
                json!({"type": "function", "function": Value::Object(function)})
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
    if let Some(max_tokens) = req.max_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
    }
    if !req.stop.is_empty() {
        body.insert("stop".into(), json!(req.stop));
    }
    if let Some(response_format) = &req.response_format {
        body.insert(
            "response_format".into(),
            map_response_format(response_format),
        );
    }

    Value::Object(body)
}

fn map_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Specific { name } => json!({"type": "function", "function": {"name": name}}),
    }
}

fn map_response_format(format: &ResponseFormat) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), json!(format.kind));
    if let Some(schema) = &format.json_schema {
        obj.insert("json_schema".into(), schema.clone());
    }
    Value::Object(obj)
}

/// Appends the OpenAI message(s) equivalent to one canonical `Message`.
///
/// This is block-driven rather than role-driven: a canonical message can mix
/// text, tool-use, and tool-result blocks (matching how e.g. Anthropic's
/// wire format models them), but OpenAI requires each of those to become its
/// own message shape (`tool_calls` on an assistant message; a standalone
/// `role: "tool"` message per result), so a single canonical `Message` may
/// expand into more than one OpenAI message.
fn append_openai_messages(message: &Message, out: &mut Vec<Value>) {
    let mut texts: Vec<&str> = Vec::new();
    let mut images: Vec<&ImageBlock> = Vec::new();
    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text(t) => texts.push(t.text.as_str()),
            ContentBlock::Image(img) => images.push(img),
            ContentBlock::ToolUse(tu) => tool_uses.push(tu),
            ContentBlock::ToolResult(tr) => tool_results.push(tr),
        }
    }

    for result in &tool_results {
        out.push(json!({
            "role": "tool",
            "tool_call_id": result.tool_use_id,
            "content": result.content,
        }));
    }

    if !tool_uses.is_empty() {
        let tool_calls: Vec<Value> = tool_uses
            .iter()
            .map(|tool_use| {
                json!({
                    "id": tool_use.id,
                    "type": "function",
                    "function": {
                        "name": tool_use.name,
                        "arguments": serde_json::to_string(&tool_use.input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    }
                })
            })
            .collect();
        let content = if texts.is_empty() {
            Value::Null
        } else {
            json!(texts.join("\n"))
        };
        let mut obj = Map::new();
        obj.insert("role".into(), json!("assistant"));
        obj.insert("content".into(), content);
        obj.insert("tool_calls".into(), Value::Array(tool_calls));
        if let Some(name) = &message.name {
            obj.insert("name".into(), json!(name));
        }
        out.push(Value::Object(obj));
        return;
    }

    if texts.is_empty() && images.is_empty() {
        // Nothing left to emit: either the message was tool-result-only
        // (already handled above) or genuinely empty.
        return;
    }

    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    let content = if images.is_empty() && texts.len() <= 1 {
        json!(texts.first().copied().unwrap_or(""))
    } else {
        let mut parts: Vec<Value> = Vec::new();
        for text in &texts {
            parts.push(json!({"type": "text", "text": text}));
        }
        for image in &images {
            parts.push(json!({"type": "image_url", "image_url": {"url": image_url(image)}}));
        }
        Value::Array(parts)
    };

    let mut obj = Map::new();
    obj.insert("role".into(), json!(role));
    obj.insert("content".into(), content);
    if let Some(name) = &message.name {
        obj.insert("name".into(), json!(name));
    }
    out.push(Value::Object(obj));
}

fn image_url(image: &ImageBlock) -> String {
    match &image.source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { data } => format!("data:{};base64,{}", image.mime, data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use morph_core::message::{
        ImageBlock, ImageSource, ToolDefinition, ToolResultBlock, ToolUseBlock,
    };
    use morph_core::request::RequestMetadata;
    use std::time::SystemTime;
    use wiremock::matchers::{body_partial_json, header, method, path};
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
    fn builds_plain_text_message_body() {
        let req = base_request("gpt-4o-mini", vec![Message::user("hello there")]);
        let body = build_request_body(&req);
        assert_eq!(body["model"], json!("gpt-4o-mini"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": "hello there"}])
        );
    }

    #[test]
    fn builds_multimodal_content_parts_array() {
        let mut msg = Message::user("describe this");
        msg.content.push(ContentBlock::Image(ImageBlock {
            mime: "image/png".to_string(),
            source: ImageSource::Base64 {
                data: "AAAA".to_string(),
            },
            rendered_by_morph: false,
        }));
        let req = base_request("gpt-4o-mini", vec![msg]);
        let body = build_request_body(&req);
        let content = &body["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], json!("text"));
        assert_eq!(content[1]["type"], json!("image_url"));
        assert_eq!(
            content[1]["image_url"]["url"],
            json!("data:image/png;base64,AAAA")
        );
    }

    #[test]
    fn maps_tool_use_and_tool_result_blocks() {
        let assistant_msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse(ToolUseBlock {
                id: "call_1".to_string(),
                name: "get_weather".to_string(),
                input: json!({"city": "Lisbon"}),
            })],
            name: None,
        };
        let tool_msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: "call_1".to_string(),
                content: "sunny".to_string(),
                is_error: false,
            })],
            name: None,
        };
        let req = base_request("gpt-4o-mini", vec![assistant_msg, tool_msg]);
        let body = build_request_body(&req);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], json!("assistant"));
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["name"],
            json!("get_weather")
        );
        assert_eq!(messages[1]["role"], json!("tool"));
        assert_eq!(messages[1]["tool_call_id"], json!("call_1"));
        assert_eq!(messages[1]["content"], json!("sunny"));
    }

    #[test]
    fn maps_tools_and_tool_choice() {
        let mut req = base_request("gpt-4o-mini", vec![Message::user("hi")]);
        req.tools.push(ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("gets the weather".to_string()),
            parameters: json!({"type": "object", "properties": {}}),
        });
        req.tool_choice = Some(ToolChoice::Specific {
            name: "get_weather".to_string(),
        });
        let body = build_request_body(&req);
        assert_eq!(body["tools"][0]["function"]["name"], json!("get_weather"));
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    #[tokio::test]
    async fn streams_text_deltas_tool_calls_usage_and_stop_reason() {
        let mock_server = MockServer::start().await;

        let sse_body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello, \"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Lisbon\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n",
        );

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(body_partial_json(json!({"stream": true})))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse_body.as_bytes(), "text/event-stream"),
            )
            .mount(&mock_server)
            .await;

        let provider = OpenAiProvider::new(
            "openai",
            mock_server.uri(),
            Some("sk-test".to_string()),
            false,
        );
        let req = base_request("gpt-4o-mini", vec![Message::user("what's the weather?")]);
        let mut stream = provider.send(req, &[]).await.expect("send should succeed");

        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event should not be an error"));
        }

        assert!(matches!(
            &events[0],
            ResponseEvent::MessageStart { id, model }
                if id == "chatcmpl-1" && model == "gpt-4o-mini"
        ));

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
                if *index == 0 && id == "call_1" && name == "get_weather"
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
            .any(|e| matches!(e, ResponseEvent::ToolCallEnd { index } if *index == 0)));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::MessageStop { stop_reason } if *stop_reason == StopReason::ToolUse
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::Usage(usage) if usage.input_tokens == 12 && usage.output_tokens == 7
        )));
    }

    #[tokio::test]
    async fn non_2xx_upstream_response_maps_to_gateway_error_upstream() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string("{\"error\":{\"message\":\"invalid api key\"}}"),
            )
            .mount(&mock_server)
            .await;

        let provider = OpenAiProvider::new(
            "openai",
            mock_server.uri(),
            Some("bad-key".to_string()),
            false,
        );
        let req = base_request("gpt-4o-mini", vec![Message::user("hi")]);
        let err = provider.send(req, &[]).await.err().expect("should fail");

        match err {
            GatewayError::Upstream { status, message } => {
                assert_eq!(status, Some(401));
                assert!(message.contains("invalid api key"));
            }
            other => panic!("expected Upstream error, got {other:?}"),
        }
    }
}
