//! HTTP-layer concerns that must run before protocol parsing: API-key auth
//! and rate limiting. Deliberately not `morph_core::traits::Middleware`
//! impls — see `morph-middleware`'s crate docs for why these live here
//! instead, operating on raw headers/the request itself rather than a
//! parsed `CanonicalRequest`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let config = state.config.borrow().clone();
    if !config.auth.enabled {
        return next.run(req).await;
    }
    match extract_key(req.headers()) {
        Some(key) if config.auth.api_keys.contains(&key) => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "missing or invalid API key").into_response(),
    }
}

pub async fn rate_limit(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let config = state.config.borrow().clone();
    if config.rate_limit.enabled {
        if let Some(limiter) = &state.rate_limiter {
            if limiter.check().is_err() {
                return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
            }
        }
    }
    next.run(req).await
}
