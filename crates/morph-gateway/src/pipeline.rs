//! The request pipeline shared by every ingress protocol: parse → request
//! middleware → request transformers → representation (detect/plan/render)
//! → cache check → provider call → (streaming: forward live; buffered:
//! collect → response transformers → response middleware → cache write) →
//! encode.
//!
//! Response transformers only run on the *buffered* path. Applying a
//! transformer (e.g. redaction) to a live token stream would mean either
//! buffering it anyway (defeating streaming's latency benefit) or risking
//! a secret pattern that spans a chunk boundary going unredacted — neither
//! is a good default, so v1 draws the line here explicitly rather than
//! quietly doing a partial job. Streaming responses still get an
//! observability hook (`tap_stream`) that captures usage/stop-reason for
//! logging/metrics/stats without buffering the text itself — except when
//! `[inspector]` is enabled, in which case it *also* buffers the text
//! (only for that copy, not the one going to the client) since the
//! dashboard has nothing useful to show otherwise. That extra buffering
//! only happens when a user has explicitly opted into the debug dashboard.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use futures::StreamExt;
use morph_core::error::GatewayError;
use morph_core::message::ContentBlock;
use morph_core::request::{CanonicalRequest, CanonicalResponse, ResponseEvent, StopReason, Usage};
use morph_core::stream::ResponseStream;
use morph_core::traits::ProtocolAdapter;

use crate::inspector::ExchangeRecorder;
use crate::representation;
use crate::state::AppState;

pub enum PipelineOutcome {
    Buffered {
        body: Vec<u8>,
        content_type: &'static str,
    },
    Streaming {
        body: axum::body::Body,
        content_type: &'static str,
    },
    Error {
        status: u16,
        body: Vec<u8>,
    },
}

fn error_outcome(protocol: &dyn ProtocolAdapter, err: &GatewayError) -> PipelineOutcome {
    tracing::warn!(error = %err, protocol = protocol.protocol_id(), "request failed");
    PipelineOutcome::Error {
        status: err.status_code(),
        body: protocol.encode_error(err),
    }
}

/// Records `sent`/`response`/`error` against `recorder` if the inspector is
/// enabled — a no-op (and, thanks to short-circuiting, no `clone()` calls
/// paid for) when it isn't.
fn record_exchange(
    state: &AppState,
    recorder: &Option<ExchangeRecorder>,
    sent: Option<&CanonicalRequest>,
    response: Option<&CanonicalResponse>,
    error: Option<&GatewayError>,
    cached: bool,
) {
    if let (Some(hub), Some(rec)) = (&state.inspector, recorder) {
        hub.record(rec.finish(
            sent.cloned(),
            response.cloned(),
            error.map(|e| e.to_string()),
            cached,
        ));
    }
}

/// Aggregates a `ResponseEvent` stream into a materialized
/// `CanonicalResponse` — used for the non-streaming (buffered) path.
async fn collect_response(
    mut stream: ResponseStream,
    model_fallback: &str,
) -> Result<CanonicalResponse, GatewayError> {
    let mut id = String::new();
    let mut model = model_fallback.to_string();
    let mut text = String::new();
    let mut tool_calls: std::collections::BTreeMap<usize, (String, String, String)> =
        Default::default();
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::EndTurn;

    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::MessageStart { id: i, model: m } => {
                id = i;
                model = m;
            }
            ResponseEvent::TextDelta { text: t, .. } => text.push_str(&t),
            ResponseEvent::ToolCallStart {
                index,
                id: tid,
                name,
            } => {
                tool_calls.insert(index, (tid, name, String::new()));
            }
            ResponseEvent::ToolCallDelta {
                index,
                partial_json,
            } => {
                if let Some(entry) = tool_calls.get_mut(&index) {
                    entry.2.push_str(&partial_json);
                }
            }
            ResponseEvent::ToolCallEnd { .. } => {}
            ResponseEvent::Usage(u) => usage = u,
            ResponseEvent::MessageStop { stop_reason: sr } => stop_reason = sr,
        }
    }

    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::text(text));
    }
    for (_, (tool_id, name, json)) in tool_calls {
        let input = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
        content.push(ContentBlock::ToolUse(morph_core::message::ToolUseBlock {
            id: tool_id,
            name,
            input,
        }));
    }

    Ok(CanonicalResponse {
        id,
        model,
        content,
        stop_reason,
        usage,
    })
}

/// Wraps a provider's response stream so that, once it's fully drained
/// (whether the client is still reading or not), a summary
/// (usage/stop-reason) is available to run middleware and stats — without
/// buffering the actual text, keeping first-byte latency for the client
/// unaffected. When the inspector is enabled, also accumulates the full
/// response text (a separate copy, never touching what's sent to the
/// client) so the dashboard has something to show for streamed requests.
fn tap_stream(
    mut inner: ResponseStream,
    state: Arc<AppState>,
    req: CanonicalRequest,
    recorder: Option<ExchangeRecorder>,
    sent: Option<CanonicalRequest>,
) -> ResponseStream {
    let started = Instant::now();
    Box::pin(async_stream::stream! {
        let mut usage = Usage::default();
        let mut stop_reason = StopReason::EndTurn;
        let mut model = req.model.clone();
        let mut text = String::new();
        let capture_text = recorder.is_some();

        while let Some(item) = inner.next().await {
            if let Ok(event) = &item {
                match event {
                    ResponseEvent::Usage(u) => usage = *u,
                    ResponseEvent::MessageStop { stop_reason: sr } => stop_reason = *sr,
                    ResponseEvent::MessageStart { model: m, .. } => model = m.clone(),
                    ResponseEvent::TextDelta { text: t, .. } if capture_text => text.push_str(t),
                    _ => {}
                }
            }
            yield item;
        }

        let summary = CanonicalResponse {
            id: String::new(),
            model,
            content: Vec::new(),
            stop_reason,
            usage,
        };
        for mw in &state.middlewares {
            let _ = mw.on_response(&req, &summary).await;
        }

        if let (Some(hub), Some(rec)) = (&state.inspector, &recorder) {
            let response = CanonicalResponse {
                content: if text.is_empty() { Vec::new() } else { vec![ContentBlock::text(text)] },
                ..summary
            };
            hub.record(rec.finish(sent, Some(response), None, false));
        }

        tracing::debug!(
            request_id = %req.metadata.request_id,
            latency_ms = started.elapsed().as_millis(),
            "streaming response finished"
        );
    })
}

pub async fn handle(
    state: Arc<AppState>,
    protocol: Arc<dyn ProtocolAdapter>,
    request_id: String,
    raw_body: Bytes,
    incoming_headers: Vec<(String, String)>,
) -> PipelineOutcome {
    let req = match protocol.parse_request(&raw_body, &request_id) {
        Ok(r) => r,
        Err(e) => return error_outcome(protocol.as_ref(), &e),
    };

    let config_snapshot = state.config.borrow().clone();
    let recorder = state.inspector.is_some().then(|| {
        ExchangeRecorder::start(
            request_id.clone(),
            protocol.protocol_id().to_string(),
            config_snapshot.default_provider.clone(),
            req.clone(),
        )
    });

    for mw in &state.middlewares {
        if let Err(e) = mw.on_request(&req).await {
            record_exchange(&state, &recorder, None, None, Some(&e), false);
            return error_outcome(protocol.as_ref(), &e);
        }
    }

    let mut req = req;
    for t in &state.request_transformers {
        req = match t.transform_request(req) {
            Ok(r) => r,
            Err(e) => {
                record_exchange(&state, &recorder, None, None, Some(&e), false);
                return error_outcome(protocol.as_ref(), &e);
            }
        };
    }

    let provider = match state.providers.get(&config_snapshot.default_provider) {
        Some(p) => p,
        None => {
            let e = GatewayError::Config(format!(
                "no provider configured under name \"{}\"",
                config_snapshot.default_provider
            ));
            record_exchange(&state, &recorder, None, None, Some(&e), false);
            return error_outcome(protocol.as_ref(), &e);
        }
    };
    let caps = provider.capabilities();

    if representation::worth_analyzing(&req) {
        let planner_cfg = representation::planner_config_from(&config_snapshot);
        representation::apply(&state, &mut req, &caps, &planner_cfg).await;
    }

    if let Some(cache) = state.cache.as_ref().filter(|_| config_snapshot.cache) {
        if !req.stream {
            if let Some(cached) = cache.get(&req) {
                for mw in &state.middlewares {
                    let _ = mw.on_response(&req, &cached).await;
                }
                record_exchange(&state, &recorder, None, Some(&cached), None, true);
                return match protocol.encode_buffered(&cached) {
                    Ok(body) => PipelineOutcome::Buffered {
                        body,
                        content_type: "application/json",
                    },
                    Err(e) => error_outcome(protocol.as_ref(), &e),
                };
            }
        }
    }

    let sent = recorder.is_some().then(|| req.clone());

    let stream = match provider.send(req.clone(), &incoming_headers).await {
        Ok(s) => s,
        Err(e) => {
            record_exchange(&state, &recorder, sent.as_ref(), None, Some(&e), false);
            return error_outcome(protocol.as_ref(), &e);
        }
    };

    if req.stream {
        let protocol_for_stream = protocol.clone();
        let tapped = tap_stream(stream, state.clone(), req.clone(), recorder, sent);
        let byte_stream = tapped.map(move |event| -> Result<Bytes, std::io::Error> {
            match event {
                Ok(ev) => match protocol_for_stream.encode_stream_event(&ev) {
                    Ok(Some(bytes)) => Ok(Bytes::from(bytes)),
                    Ok(None) => Ok(Bytes::new()),
                    Err(e) => Ok(Bytes::from(protocol_for_stream.encode_error(&e))),
                },
                Err(e) => Ok(Bytes::from(protocol_for_stream.encode_error(&e))),
            }
        });
        let content_type = if protocol.protocol_id() == "ollama" {
            "application/x-ndjson"
        } else {
            "text/event-stream"
        };
        PipelineOutcome::Streaming {
            body: axum::body::Body::from_stream(byte_stream),
            content_type,
        }
    } else {
        let resp = match collect_response(stream, &req.model).await {
            Ok(r) => r,
            Err(e) => {
                record_exchange(&state, &recorder, sent.as_ref(), None, Some(&e), false);
                return error_outcome(protocol.as_ref(), &e);
            }
        };
        let mut resp = resp;
        for t in &state.response_transformers {
            resp = match t.transform_response(resp) {
                Ok(r) => r,
                Err(e) => {
                    record_exchange(&state, &recorder, sent.as_ref(), None, Some(&e), false);
                    return error_outcome(protocol.as_ref(), &e);
                }
            };
        }
        for mw in &state.middlewares {
            if let Err(e) = mw.on_response(&req, &resp).await {
                tracing::warn!(error = %e, "response middleware failed (non-fatal)");
            }
        }
        if let Some(cache) = state.cache.as_ref().filter(|_| config_snapshot.cache) {
            cache.put(&req, resp.clone());
        }
        record_exchange(&state, &recorder, sent.as_ref(), Some(&resp), None, false);
        match protocol.encode_buffered(&resp) {
            Ok(body) => PipelineOutcome::Buffered {
                body,
                content_type: "application/json",
            },
            Err(e) => error_outcome(protocol.as_ref(), &e),
        }
    }
}
