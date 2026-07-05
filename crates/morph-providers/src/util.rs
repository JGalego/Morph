//! Small pieces shared by every wire-format-specific provider adapter:
//! turning a non-2xx upstream response into a `GatewayError::Upstream`, and
//! turning a `reqwest` transport failure into `Timeout` vs `Upstream`
//! depending on what actually happened. Kept here instead of duplicated in
//! `openai.rs`/`anthropic.rs` since both adapters send requests the same way
//! even though their request/response *bodies* differ completely.

use std::time::Instant;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
