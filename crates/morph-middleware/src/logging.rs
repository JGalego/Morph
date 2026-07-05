use async_trait::async_trait;
use morph_core::error::GatewayError;
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use morph_core::traits::Middleware;

/// Structured request/response logging via `tracing`. Prompt/response
/// *content* is only emitted when `log_prompts` is true — off by default,
/// per the project's "no prompt logging by default" security goal. Shape
/// (message count, model, token counts, latency) is always logged, since
/// that's what operators need for debugging without ever seeing user data.
pub struct LoggingMiddleware {
    log_prompts: bool,
}

impl LoggingMiddleware {
    pub fn new(log_prompts: bool) -> Self {
        LoggingMiddleware { log_prompts }
    }
}

#[async_trait]
impl Middleware for LoggingMiddleware {
    fn name(&self) -> &str {
        "logging"
    }

    async fn on_request(&self, req: &CanonicalRequest) -> Result<(), GatewayError> {
        if self.log_prompts {
            tracing::info!(
                request_id = %req.metadata.request_id,
                protocol = %req.metadata.ingress_protocol,
                model = %req.model,
                stream = req.stream,
                messages = ?req.messages,
                "request received"
            );
        } else {
            tracing::info!(
                request_id = %req.metadata.request_id,
                protocol = %req.metadata.ingress_protocol,
                model = %req.model,
                stream = req.stream,
                message_count = req.messages.len(),
                "request received"
            );
        }
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
            .map(|d| d.as_millis())
            .unwrap_or(0);

        if self.log_prompts {
            tracing::info!(
                request_id = %req.metadata.request_id,
                stop_reason = ?resp.stop_reason,
                latency_ms,
                input_tokens = resp.usage.input_tokens,
                output_tokens = resp.usage.output_tokens,
                content = ?resp.content,
                "response sent"
            );
        } else {
            tracing::info!(
                request_id = %req.metadata.request_id,
                stop_reason = ?resp.stop_reason,
                latency_ms,
                input_tokens = resp.usage.input_tokens,
                output_tokens = resp.usage.output_tokens,
                "response sent"
            );
        }
        Ok(())
    }
}
