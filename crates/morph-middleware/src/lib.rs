//! Cross-cutting request/response concerns for Morph.
//!
//! `Middleware` trait impls here (`LoggingMiddleware`, `MetricsMiddleware`,
//! `RequestRecordingMiddleware`) are pure before/after hooks over a
//! `CanonicalRequest`/`CanonicalResponse` pair. Concerns that need to change
//! control flow rather than just observe it live elsewhere on purpose:
//! - **Redaction** is a `Transformer` (`RedactionTransformer`) since it
//!   modifies the request/response, which is what `Transformer` is for.
//! - **Caching** (`ResponseCache`) is a standalone type `morph-gateway`
//!   calls directly around its provider-call step, since a cache hit must
//!   skip the provider call entirely — something `Middleware::on_request`
//!   returning `Result<(), GatewayError>` has no way to express.
//! - **Auth** and **rate limiting** are HTTP-layer concerns implemented in
//!   `morph-gateway` with `tower`/`tower_governor` layers, since they need
//!   the raw request (headers, client IP) before protocol parsing even
//!   happens.
//! - **Compression** is `tower_http::compression::CompressionLayer`, wired
//!   directly into `morph-gateway`'s axum router — no reason to reinvent it.

mod cache;
mod logging;
mod metrics_mw;
mod recording;
mod redaction;
mod stats;

pub use cache::{cache_key, ResponseCache};
pub use logging::LoggingMiddleware;
pub use metrics_mw::MetricsMiddleware;
pub use recording::RequestRecordingMiddleware;
pub use redaction::RedactionTransformer;
pub use stats::{Aggregate, InMemoryStatsSink};
