use async_trait::async_trait;
use morph_core::error::GatewayError;
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use morph_core::traits::Middleware;

/// Emits Prometheus counters/histograms (via the `metrics` crate facade —
/// `morph-gateway` installs the actual Prometheus recorder/exporter) for
/// every request. Kept deliberately separate from `InMemoryStatsSink`
/// (`stats.rs`): this middleware is operational (request-rate/latency
/// dashboards), while `StatsSink` is specifically about which
/// content-kind/representation pairs are chosen, feeding the adaptive
/// optimization stretch goal.
pub struct MetricsMiddleware;

#[async_trait]
impl Middleware for MetricsMiddleware {
    fn name(&self) -> &str {
        "metrics"
    }

    async fn on_request(&self, req: &CanonicalRequest) -> Result<(), GatewayError> {
        metrics::counter!(
            "morph_requests_total",
            "protocol" => req.metadata.ingress_protocol.clone(),
            "model" => req.model.clone(),
        )
        .increment(1);
        Ok(())
    }

    async fn on_response(
        &self,
        req: &CanonicalRequest,
        resp: &CanonicalResponse,
    ) -> Result<(), GatewayError> {
        let latency_ms = req
            .metadata
            .received_at
            .elapsed()
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);

        metrics::histogram!(
            "morph_request_latency_ms",
            "protocol" => req.metadata.ingress_protocol.clone(),
        )
        .record(latency_ms);

        metrics::counter!(
            "morph_tokens_total",
            "direction" => "input",
            "model" => req.model.clone(),
        )
        .increment(resp.usage.input_tokens as u64);
        metrics::counter!(
            "morph_tokens_total",
            "direction" => "output",
            "model" => req.model.clone(),
        )
        .increment(resp.usage.output_tokens as u64);

        metrics::counter!(
            "morph_responses_total",
            "stop_reason" => format!("{:?}", resp.stop_reason),
        )
        .increment(1);

        Ok(())
    }
}
