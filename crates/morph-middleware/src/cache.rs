use std::time::Duration;

use moka::sync::Cache;
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use serde::Serialize;

/// Response cache keyed by the semantically-relevant subset of a request —
/// everything except `metadata` (request_id/timestamp are unique per call
/// by construction) and `stream` (a cached answer serves either a streaming
/// or buffered client equally; `morph-gateway` is responsible for replaying
/// it in whichever shape the client asked for).
///
/// This is intentionally *not* a `Middleware` impl: caching needs to
/// short-circuit the pipeline (skip the provider call entirely on a hit),
/// which doesn't fit the before/after hook shape of `Middleware::on_request`/
/// `on_response`. `morph-gateway` calls `get`/`put` directly around its
/// provider-call step instead.
pub struct ResponseCache {
    inner: Cache<String, CanonicalResponse>,
}

#[derive(Serialize)]
struct CacheKeyParts<'a> {
    model: &'a str,
    messages: &'a [morph_core::message::Message],
    system: &'a Option<String>,
    tools: &'a [morph_core::message::ToolDefinition],
    tool_choice: &'a Option<morph_core::message::ToolChoice>,
    temperature: &'a Option<f32>,
    top_p: &'a Option<f32>,
    max_tokens: &'a Option<u32>,
    stop: &'a [String],
    response_format: &'a Option<morph_core::request::ResponseFormat>,
}

pub fn cache_key(req: &CanonicalRequest) -> String {
    let parts = CacheKeyParts {
        model: &req.model,
        messages: &req.messages,
        system: &req.system,
        tools: &req.tools,
        tool_choice: &req.tool_choice,
        temperature: &req.temperature,
        top_p: &req.top_p,
        max_tokens: &req.max_tokens,
        stop: &req.stop,
        response_format: &req.response_format,
    };
    // Falls back to a key that can never collide with a real cache entry
    // rather than panicking — a request whose relevant fields somehow fail
    // to serialize just never gets a cache hit.
    serde_json::to_string(&parts)
        .unwrap_or_else(|_| format!("unserializable:{}", req.metadata.request_id))
}

impl ResponseCache {
    pub fn new(max_entries: u64, ttl: Duration) -> Self {
        ResponseCache {
            inner: Cache::builder()
                .max_capacity(max_entries)
                .time_to_live(ttl)
                .build(),
        }
    }

    pub fn get(&self, req: &CanonicalRequest) -> Option<CanonicalResponse> {
        self.inner.get(&cache_key(req))
    }

    pub fn put(&self, req: &CanonicalRequest, resp: CanonicalResponse) {
        self.inner.insert(cache_key(req), resp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::message::Message;
    use morph_core::request::{RequestMetadata, StopReason, Usage};
    use std::time::SystemTime;

    fn req(model: &str, text: &str, request_id: &str) -> CanonicalRequest {
        CanonicalRequest {
            model: model.to_string(),
            messages: vec![Message::user(text)],
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
                request_id: request_id.to_string(),
                ingress_protocol: "openai_chat".to_string(),
                received_at: SystemTime::now(),
            },
            extra: serde_json::Value::Null,
        }
    }

    fn resp() -> CanonicalResponse {
        CanonicalResponse {
            id: "resp-1".to_string(),
            model: "gpt-4o".to_string(),
            content: vec![morph_core::message::ContentBlock::text("cached answer")],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    #[test]
    fn identical_requests_hit_despite_different_request_ids() {
        let cache = ResponseCache::new(100, Duration::from_secs(60));
        let a = req("gpt-4o", "hello", "req-a");
        let b = req("gpt-4o", "hello", "req-b");

        assert!(cache.get(&a).is_none());
        cache.put(&a, resp());
        assert!(
            cache.get(&b).is_some(),
            "same logical request should hit regardless of request_id"
        );
    }

    #[test]
    fn different_prompts_do_not_collide() {
        let cache = ResponseCache::new(100, Duration::from_secs(60));
        cache.put(&req("gpt-4o", "hello", "req-a"), resp());
        assert!(cache.get(&req("gpt-4o", "goodbye", "req-b")).is_none());
    }
}
