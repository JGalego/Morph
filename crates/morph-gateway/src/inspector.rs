//! In-memory "what did Morph get, what did it send" recorder — the backend
//! for the optional live web dashboard at `/_inspector` (see `routes.rs`).
//!
//! Gated entirely behind `[inspector] enabled` (`morph_config::
//! InspectorConfig`): when disabled, `AppState.inspector` is `None` and
//! nothing in `pipeline::handle` even constructs an `Exchange`, let alone
//! clones a `CanonicalRequest`/`CanonicalResponse` into one — zero capture,
//! zero overhead. When enabled, this holds full prompt/response content in
//! memory (including any rendered images, as base64 inside the captured
//! `CanonicalRequest`), bounded to `max_events` — a debugging tool, not
//! something to run on a publicly reachable instance without separately
//! securing it. See docs/ARCHITECTURE.md.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use morph_core::request::{CanonicalRequest, CanonicalResponse};
use serde::Serialize;
use tokio::sync::broadcast;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One captured request/response cycle. `received` is the parsed request
/// exactly as the client sent it, before any transformation; `sent` is the
/// request exactly as it went to the provider (after redaction and any
/// rendering) — comparing the two is the whole point. `sent`/`response` are
/// `None` when the pipeline never reached that stage (e.g. a request
/// rejected by middleware before any provider call was attempted).
#[derive(Debug, Clone, Serialize)]
pub struct Exchange {
    pub id: String,
    pub timestamp_ms: u64,
    pub protocol: String,
    pub provider: String,
    pub latency_ms: u64,
    /// True if this exchange was served from `ResponseCache` — `sent` is
    /// `None` in that case, since nothing was actually sent this time.
    pub cached: bool,
    pub received: CanonicalRequest,
    pub sent: Option<CanonicalRequest>,
    pub response: Option<CanonicalResponse>,
    pub error: Option<String>,
}

/// Carries the one immutable fact (`received`) and the bookkeeping
/// (id/protocol/provider/start time) needed to finish an `Exchange` from
/// whichever point in `pipeline::handle` the request's outcome becomes
/// known — success, provider failure, or rejection at an earlier stage.
pub struct ExchangeRecorder {
    id: String,
    protocol: String,
    provider: String,
    received: CanonicalRequest,
    started_at: Instant,
}

impl ExchangeRecorder {
    pub fn start(
        id: String,
        protocol: String,
        provider: String,
        received: CanonicalRequest,
    ) -> Self {
        ExchangeRecorder {
            id,
            protocol,
            provider,
            received,
            started_at: Instant::now(),
        }
    }

    pub fn finish(
        &self,
        sent: Option<CanonicalRequest>,
        response: Option<CanonicalResponse>,
        error: Option<String>,
        cached: bool,
    ) -> Exchange {
        Exchange {
            id: self.id.clone(),
            timestamp_ms: now_ms(),
            protocol: self.protocol.clone(),
            provider: self.provider.clone(),
            latency_ms: self.started_at.elapsed().as_millis() as u64,
            cached,
            received: self.received.clone(),
            sent,
            response,
            error,
        }
    }
}

/// Bounded ring buffer of recent exchanges, plus a broadcast channel so the
/// dashboard's SSE endpoint can push new ones live instead of polling.
pub struct InspectorHub {
    buffer: Mutex<VecDeque<Exchange>>,
    max_events: usize,
    tx: broadcast::Sender<Exchange>,
}

impl InspectorHub {
    pub fn new(max_events: usize) -> Self {
        let (tx, _rx) = broadcast::channel(32);
        InspectorHub {
            buffer: Mutex::new(VecDeque::with_capacity(max_events.max(1))),
            max_events: max_events.max(1),
            tx,
        }
    }

    /// Records `exchange`, evicting the oldest entry if the buffer is full,
    /// and broadcasts it to any live dashboard viewers. Broadcasting to zero
    /// subscribers is not an error — the dashboard doesn't have to be open.
    pub fn record(&self, exchange: Exchange) {
        {
            let mut buffer = self.buffer.lock().expect("inspector buffer lock poisoned");
            buffer.push_front(exchange.clone());
            while buffer.len() > self.max_events {
                buffer.pop_back();
            }
        }
        let _ = self.tx.send(exchange);
    }

    /// Most recent first.
    pub fn snapshot(&self) -> Vec<Exchange> {
        self.buffer
            .lock()
            .expect("inspector buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Exchange> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::message::Message;
    use morph_core::request::RequestMetadata;

    fn sample_request(id: &str) -> CanonicalRequest {
        CanonicalRequest {
            model: "test-model".to_string(),
            messages: vec![Message::user("hi")],
            system: None,
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: false,
            stop: vec![],
            response_format: None,
            reasoning: None,
            metadata: RequestMetadata {
                request_id: id.to_string(),
                ingress_protocol: "test".to_string(),
                received_at: SystemTime::now(),
            },
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn records_and_snapshots_most_recent_first() {
        let hub = InspectorHub::new(10);
        for i in 0..3 {
            let rec = ExchangeRecorder::start(
                format!("req-{i}"),
                "openai_chat".to_string(),
                "openai".to_string(),
                sample_request(&format!("req-{i}")),
            );
            hub.record(rec.finish(None, None, None, false));
        }

        let snapshot = hub.snapshot();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].id, "req-2");
        assert_eq!(snapshot[2].id, "req-0");
    }

    #[test]
    fn evicts_oldest_once_over_capacity() {
        let hub = InspectorHub::new(2);
        for i in 0..5 {
            let rec = ExchangeRecorder::start(
                format!("req-{i}"),
                "openai_chat".to_string(),
                "openai".to_string(),
                sample_request(&format!("req-{i}")),
            );
            hub.record(rec.finish(None, None, None, false));
        }

        let snapshot = hub.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].id, "req-4");
        assert_eq!(snapshot[1].id, "req-3");
    }

    #[test]
    fn subscribers_receive_recorded_exchanges_live() {
        let hub = InspectorHub::new(10);
        let mut rx = hub.subscribe();

        let rec = ExchangeRecorder::start(
            "req-live".to_string(),
            "anthropic_messages".to_string(),
            "anthropic".to_string(),
            sample_request("req-live"),
        );
        hub.record(rec.finish(None, None, Some("boom".to_string()), false));

        let received = rx.try_recv().expect("expected a broadcasted exchange");
        assert_eq!(received.id, "req-live");
        assert_eq!(received.error, Some("boom".to_string()));
    }
}
