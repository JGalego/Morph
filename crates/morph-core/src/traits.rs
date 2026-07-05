use async_trait::async_trait;

use crate::capabilities::Capabilities;
use crate::content::{ContentKind, DetectedContent, RequestAnalysis};
use crate::error::GatewayError;
use crate::representation::{PlannerConfig, RenderOptions, RenderedAsset, RepresentationPlan};
use crate::request::{CanonicalRequest, CanonicalResponse};
use crate::stats::StatsEvent;
use crate::stream::ResponseStream;

/// Speaks a provider's native wire protocol. Implementing this trait — and
/// nothing else — is sufficient to add a new backend LLM to Morph.
///
/// Provider adapters are deliberately *not* part of the WASM plugin surface:
/// they hold API keys and make outbound network calls, so they stay native,
/// compiled-in code that gets normal Rust review, not sandboxed guest code.
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Stable identifier used in config (`provider = "..."`) and stats.
    fn name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Send a canonical request upstream. Always returns a stream — see
    /// `ResponseEvent`'s doc comment for why non-streaming providers still
    /// implement this by emitting one synchronous burst of events.
    ///
    /// `incoming_headers` is the raw header list from the client's original
    /// HTTP request to Morph, given to every provider (not carried on
    /// `CanonicalRequest` itself, so it never ends up in a log line or a
    /// cache key by accident). Most adapters ignore it and authenticate
    /// with their own configured credential; an adapter that supports
    /// `passthrough_auth` uses it instead, to forward whatever credential
    /// the client already has (e.g. an OAuth-backed subscription login)
    /// rather than requiring Morph to have its own API key.
    async fn send(
        &self,
        req: CanonicalRequest,
        incoming_headers: &[(String, String)],
    ) -> Result<ResponseStream, GatewayError>;
}

/// Translates between one client-facing wire protocol (OpenAI Chat
/// Completions, Anthropic Messages, Ollama, ...) and `CanonicalRequest` /
/// `CanonicalResponse`. Protocol adapters never talk to `ProviderAdapter`s
/// directly; the canonical types are the only seam between them.
pub trait ProtocolAdapter: Send + Sync {
    /// Stable identifier, e.g. "openai_chat", "anthropic_messages", "ollama".
    fn protocol_id(&self) -> &str;

    /// `request_id` is minted by the gateway the instant a connection is
    /// accepted (before the protocol is even known), so tracing/logging can
    /// key off it from the very first byte.
    fn parse_request(&self, raw: &[u8], request_id: &str)
        -> Result<CanonicalRequest, GatewayError>;

    /// Encode one response event as a protocol-native SSE chunk (or
    /// equivalent framed message), or `None` if this event type carries no
    /// wire representation in this protocol (e.g. a bare `Usage` event for a
    /// protocol that only reports usage at stream end).
    fn encode_stream_event(
        &self,
        event: &crate::request::ResponseEvent,
    ) -> Result<Option<Vec<u8>>, GatewayError>;

    /// Encode a fully materialized response as a single JSON body, for
    /// clients that did not request streaming.
    fn encode_buffered(&self, resp: &CanonicalResponse) -> Result<Vec<u8>, GatewayError>;

    /// Encode an error in this protocol's expected error shape.
    fn encode_error(&self, err: &GatewayError) -> Vec<u8>;
}

/// Detects the structural kind of a piece of text. The built-in
/// implementation lives in `morph-detect`; WASM plugins may register
/// additional classifiers for kinds Morph doesn't know natively.
pub trait Classifier: Send + Sync {
    fn name(&self) -> &str;

    /// Candidate `(kind, confidence)` pairs, most confident first. Confidence
    /// is in `[0, 1]`. An empty result means "no opinion" — the caller falls
    /// back to `ContentKind::PlainText`.
    fn classify(&self, text: &str) -> Vec<(ContentKind, f32)>;
}

/// Renders one detected content segment into a raster/vector asset.
/// Rendering is modeled as synchronous, CPU-bound work — callers that need
/// to avoid blocking an async executor should run it via
/// `tokio::task::spawn_blocking`.
pub trait Renderer: Send + Sync {
    fn name(&self) -> &str;

    fn supports(&self, kind: ContentKind) -> bool;

    fn render(
        &self,
        content: &DetectedContent,
        opts: &RenderOptions,
    ) -> Result<RenderedAsset, GatewayError>;
}

/// Decides, per request, which segments get rendered and how. The default
/// heuristic implementation lives in `morph-detect`; this trait exists so it
/// can be swapped for an ML-based planner later (see the stretch goal in the
/// project spec) without touching `morph-gateway`.
pub trait RepresentationPlanner: Send + Sync {
    fn plan(
        &self,
        analysis: &RequestAnalysis,
        caps: &Capabilities,
        cfg: &PlannerConfig,
    ) -> RepresentationPlan;
}

/// Transforms a canonical request or response. Used for prompt optimization,
/// redaction, and WASM transformer plugins. Modeled as synchronous pure
/// computation — a transformer must not perform network I/O, which is the
/// property that makes it safe to run inside a WASM sandbox.
pub trait Transformer: Send + Sync {
    fn name(&self) -> &str;

    fn transform_request(&self, req: CanonicalRequest) -> Result<CanonicalRequest, GatewayError>;

    fn transform_response(
        &self,
        resp: CanonicalResponse,
    ) -> Result<CanonicalResponse, GatewayError>;
}

/// Per-request middleware hook. Unlike `Transformer`, middleware is native
/// only (not a WASM plugin surface) since it commonly needs real I/O
/// (writing logs, checking a rate-limit store, recording to disk).
#[async_trait]
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;

    async fn on_request(&self, req: &CanonicalRequest) -> Result<(), GatewayError> {
        let _ = req;
        Ok(())
    }

    /// `req` is the same request `on_request` already saw for this
    /// exchange, passed again so middleware can correlate/measure latency
    /// (e.g. via `req.metadata.received_at`) without needing its own
    /// request-scoped state.
    async fn on_response(
        &self,
        req: &CanonicalRequest,
        resp: &CanonicalResponse,
    ) -> Result<(), GatewayError> {
        let _ = (req, resp);
        Ok(())
    }
}

/// Sink for adaptive-optimization telemetry. `record` must be non-blocking
/// (implementations typically push onto a channel) since it is called on the
/// hot path after every request.
pub trait StatsSink: Send + Sync {
    fn record(&self, event: StatsEvent);
}
