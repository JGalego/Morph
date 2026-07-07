//! Small pieces shared by every wire-format-specific provider adapter:
//! turning a non-2xx upstream response into a `GatewayError::Upstream`, and
//! turning a `reqwest` transport failure into `Timeout` vs `Upstream`
//! depending on what actually happened. Kept here instead of duplicated in
//! `openai.rs`/`anthropic.rs` since both adapters send requests the same way
//! even though their request/response *bodies* differ completely.

use std::time::{Duration, Instant};

use morph_config::RetryConfig;
use morph_core::error::GatewayError;

/// Headers that must never be blindly forwarded from the client's original
/// request to the upstream call: hop-by-hop headers (meaningless or actively
/// wrong to repeat on a new connection) and body-describing headers (which
/// no longer match — Morph is sending its own re-encoded JSON body, not a
/// byte-for-byte copy of what the client sent).
///
/// Deliberately a deny-list, not an allow-list: `passthrough_auth` exists
/// specifically for credentials Morph doesn't know the shape of in advance
/// (e.g. whatever header(s) an OAuth-backed subscription login sends), so
/// guessing at an allow-list of "known auth headers" would silently break
/// exactly the case this feature is for.
const NON_FORWARDABLE_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "proxy-connection",
    "accept-encoding",
    "te",
    "expect",
];

/// Filters `incoming_headers` down to the set safe to replay verbatim on the
/// upstream request, for `passthrough_auth` providers.
pub(crate) fn passthrough_headers(incoming_headers: &[(String, String)]) -> Vec<(&str, &str)> {
    incoming_headers
        .iter()
        .filter(|(name, _)| {
            !NON_FORWARDABLE_HEADERS
                .iter()
                .any(|denied| name.eq_ignore_ascii_case(denied))
        })
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Sends `builder`, retrying transient failures (HTTP 429, 5xx, or a
/// transport timeout — never 4xx client errors, which would just fail the
/// same way again) according to `retry`, with exponential backoff between
/// attempts.
///
/// Retrying means re-sending the same request body, so each attempt after
/// the first uses `RequestBuilder::try_clone`. That only fails for a
/// streaming request body, which none of Morph's provider adapters send
/// (they always build the body via `.json(...)`); if it ever did, this falls
/// back to a single, unretried attempt rather than erroring out.
///
/// Callers are left with a `reqwest::Response` whose body has *not* been
/// consumed on the success path, so it can be turned into a byte stream for
/// SSE parsing.
pub(crate) async fn send_request(
    builder: reqwest::RequestBuilder,
    retry: &RetryConfig,
) -> Result<reqwest::Response, GatewayError> {
    let original = builder;
    let mut attempt: u32 = 0;

    loop {
        let can_retry = retry.enabled && attempt < retry.max_retries;
        let this_attempt = can_retry.then(|| original.try_clone()).flatten();

        let result = match this_attempt {
            Some(clone) => send_once(clone).await,
            None => return send_once(original).await,
        };

        match result {
            Ok(response) => return Ok(response),
            Err(err) if is_retryable(&err) => {
                attempt += 1;
                let delay = backoff_delay(retry, attempt);
                tracing::warn!(
                    attempt,
                    max_retries = retry.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %err,
                    "retrying upstream request after transient failure"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Whether `err` is worth retrying: a rate limit, a server-side (5xx) error,
/// or a network timeout. Never a 4xx other than 429 — those (bad request,
/// unauthorized, ...) will fail identically on every retry.
fn is_retryable(err: &GatewayError) -> bool {
    match err {
        GatewayError::Timeout(_) => true,
        GatewayError::Upstream { status: Some(429), .. } => true,
        GatewayError::Upstream {
            status: Some(status),
            ..
        } => (500..600).contains(status),
        _ => false,
    }
}

/// `initial_backoff_ms * 2^(attempt - 1)`, capped at `max_backoff_ms`.
fn backoff_delay(retry: &RetryConfig, attempt: u32) -> Duration {
    let scale = 1u64 << attempt.saturating_sub(1).min(32);
    let delay_ms = retry.initial_backoff_ms.saturating_mul(scale);
    Duration::from_millis(delay_ms.min(retry.max_backoff_ms))
}

/// Sends `builder` once, mapping transport-level failures (timeout, DNS,
/// TLS, connection reset, ...) to `GatewayError::Timeout`/`GatewayError::Upstream`,
/// then checks the HTTP status and reads the body into an `Upstream` error
/// for any non-2xx response.
async fn send_once(builder: reqwest::RequestBuilder) -> Result<reqwest::Response, GatewayError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn forwards_unrecognized_headers_verbatim() {
        // The exact point of a deny-list: an auth-shaped header this crate
        // has never heard of still gets through unchanged.
        let incoming = pairs(&[
            ("authorization", "Bearer sk-whatever"),
            ("anthropic-beta", "oauth-2026-01-01"),
            ("x-some-future-auth-scheme", "opaque-token"),
        ]);
        let forwarded = passthrough_headers(&incoming);
        assert_eq!(forwarded.len(), 3);
        assert!(forwarded.contains(&("authorization", "Bearer sk-whatever")));
        assert!(forwarded.contains(&("anthropic-beta", "oauth-2026-01-01")));
        assert!(forwarded.contains(&("x-some-future-auth-scheme", "opaque-token")));
    }

    #[test]
    fn drops_hop_by_hop_and_body_describing_headers() {
        let incoming = pairs(&[
            ("Host", "localhost:8080"),
            ("Content-Length", "42"),
            ("Content-Type", "application/json"),
            ("Connection", "keep-alive"),
            ("Authorization", "Bearer token"),
        ]);
        let forwarded = passthrough_headers(&incoming);
        assert_eq!(forwarded, vec![("Authorization", "Bearer token")]);
    }

    #[test]
    fn header_name_matching_is_case_insensitive() {
        let incoming = pairs(&[("HOST", "example.com"), ("hOsT", "example.com")]);
        assert!(passthrough_headers(&incoming).is_empty());
    }

    /// Backoff kept at 1ms so these tests run fast; the delay's shape is
    /// covered separately by `backoff_delay_*` tests below.
    fn fast_retry(max_retries: u32) -> RetryConfig {
        RetryConfig {
            enabled: true,
            max_retries,
            initial_backoff_ms: 1,
            max_backoff_ms: 1,
        }
    }

    #[tokio::test]
    async fn retries_on_429_and_succeeds_once_upstream_recovers() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/x", mock_server.uri()));
        let response = send_request(builder, &fast_retry(2))
            .await
            .expect("should succeed once the upstream 429s are exhausted");

        assert!(response.status().is_success());
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn gives_up_and_returns_the_error_once_retries_are_exhausted() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/x", mock_server.uri()));
        let err = send_request(builder, &fast_retry(1))
            .await
            .expect_err("every attempt 429s, so this must ultimately fail");

        assert!(matches!(
            err,
            GatewayError::Upstream {
                status: Some(429),
                ..
            }
        ));
        // 1 initial attempt + 1 retry.
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn does_not_retry_a_non_transient_client_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&mock_server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/x", mock_server.uri()));
        let err = send_request(builder, &fast_retry(2))
            .await
            .expect_err("400 should fail");

        assert!(matches!(
            err,
            GatewayError::Upstream {
                status: Some(400),
                ..
            }
        ));
        // A bad request fails the same way every time, so it's not retried.
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disabled_retry_makes_only_a_single_attempt_even_on_429() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let builder = reqwest::Client::new().get(format!("{}/x", mock_server.uri()));
        let retry = RetryConfig {
            enabled: false,
            ..fast_retry(3)
        };
        let err = send_request(builder, &retry)
            .await
            .expect_err("429 should still fail with retry disabled");

        assert!(matches!(
            err,
            GatewayError::Upstream {
                status: Some(429),
                ..
            }
        ));
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn backoff_delay_doubles_each_attempt_up_to_the_cap() {
        let retry = RetryConfig {
            enabled: true,
            max_retries: 5,
            initial_backoff_ms: 100,
            max_backoff_ms: 350,
        };
        assert_eq!(backoff_delay(&retry, 1), Duration::from_millis(100));
        assert_eq!(backoff_delay(&retry, 2), Duration::from_millis(200));
        assert_eq!(backoff_delay(&retry, 3), Duration::from_millis(350)); // capped, would be 400
        assert_eq!(backoff_delay(&retry, 4), Duration::from_millis(350));
    }
}
