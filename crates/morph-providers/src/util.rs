//! Small pieces shared by every wire-format-specific provider adapter:
//! turning a non-2xx upstream response into a `GatewayError::Upstream`, and
//! turning a `reqwest` transport failure into `Timeout` vs `Upstream`
//! depending on what actually happened. Kept here instead of duplicated in
//! `openai.rs`/`anthropic.rs` since both adapters send requests the same way
//! even though their request/response *bodies* differ completely.

use std::time::Instant;

use morph_core::error::GatewayError;

/// Sends `builder`, mapping transport-level failures (timeout, DNS, TLS,
/// connection reset, ...) to `GatewayError::Timeout`/`GatewayError::Upstream`,
/// then checks the HTTP status and reads the body into an `Upstream` error
/// for any non-2xx response.
///
/// Callers are left with a `reqwest::Response` whose body has *not* been
/// consumed on the success path, so it can be turned into a byte stream for
/// SSE parsing.
pub(crate) async fn send_request(
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, GatewayError> {
    let started_at = Instant::now();
    let response = builder.send().await.map_err(|err| {
        if err.is_timeout() {
            GatewayError::Timeout(started_at.elapsed().as_millis() as u64)
        } else {
            GatewayError::Upstream {
                status: err.status().map(|s| s.as_u16()),
                message: err.to_string(),
            }
        }
    })?;

    check_status(response).await
}

/// Rejects non-2xx responses, reading the body text as the error message
/// (most providers put a JSON error body there that's useful for debugging
/// even though we don't attempt to parse its shape).
async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, GatewayError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let code = status.as_u16();
    let body = response.text().await.unwrap_or_default();
    Err(GatewayError::Upstream {
        status: Some(code),
        message: body,
    })
}
