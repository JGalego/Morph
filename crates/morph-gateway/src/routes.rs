use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::pipeline::{self, PipelineOutcome};
use crate::state::AppState;

const INSPECTOR_DISABLED_MESSAGE: &str =
    "The inspector is disabled. Set [inspector] enabled = true in morph.toml and restart.";

/// Header values are forwarded as-is (lossily, for anything not valid
/// UTF-8) rather than dropped — `passthrough_auth` providers need the exact
/// bytes a client sent, and non-UTF-8 header values are rare enough in
/// practice (this is virtually always ASCII tokens/base64/JWTs) that lossy
/// conversion is a reasonable trade for not having to plumb raw bytes
/// through the whole pipeline.
fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

async fn dispatch(
    state: Arc<AppState>,
    protocol_id: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(protocol) = state.protocols.get(protocol_id) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "protocol not registered").into_response();
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let headers = header_pairs(&headers);

    match pipeline::handle(state, protocol, request_id, body, headers).await {
        PipelineOutcome::Buffered { body, content_type } => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        PipelineOutcome::Streaming { body, content_type } => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .header("cache-control", "no-cache")
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        PipelineOutcome::Error { status, body } => Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

pub async fn openai_chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch(state, "openai_chat", headers, body).await
}

pub async fn anthropic_messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch(state, "anthropic_messages", headers, body).await
}

pub async fn ollama_chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch(state, "ollama", headers, body).await
}

pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    let body = serde_json::json!({
        "status": "ok",
        "uptime_seconds": state.started_at.elapsed().as_secs(),
        "providers": state.providers.names(),
        "plugins_loaded": state.plugin_infos.len(),
    });
    (StatusCode::OK, Json(body)).into_response()
}

pub async fn metrics(Extension(handle): Extension<PrometheusHandle>) -> impl IntoResponse {
    handle.render()
}

/// Returns the configured providers' default models in the shape of
/// OpenAI's `/v1/models` — several clients (Continue, LibreChat, and most
/// OpenAI-SDK-based tools) probe this at startup before sending a single
/// chat request, and refuse to proceed if it 404s.
pub async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config.borrow().clone();
    let data: Vec<_> = config
        .providers
        .values()
        .filter_map(|p| p.default_model.as_ref())
        .map(|model| serde_json::json!({"id": model, "object": "model", "owned_by": "morph"}))
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"object": "list", "data": data})),
    )
        .into_response()
}

const INSPECTOR_HTML: &str = include_str!("inspector.html");

/// Serves the live dashboard page — see `docs/ARCHITECTURE.md` for why this
/// (and the two endpoints below) only exist in any meaningful sense when
/// `[inspector] enabled = true`.
pub async fn inspector_page(State(state): State<Arc<AppState>>) -> Response {
    if state.inspector.is_none() {
        return (StatusCode::NOT_FOUND, INSPECTOR_DISABLED_MESSAGE).into_response();
    }
    Html(INSPECTOR_HTML).into_response()
}

/// The dashboard's initial-load snapshot — most recent exchange first.
pub async fn inspector_events(State(state): State<Arc<AppState>>) -> Response {
    let Some(hub) = &state.inspector else {
        return (StatusCode::NOT_FOUND, INSPECTOR_DISABLED_MESSAGE).into_response();
    };
    (StatusCode::OK, Json(hub.snapshot())).into_response()
}

/// Pushes each newly recorded exchange to the dashboard live via SSE, so it
/// updates as requests happen instead of requiring a page refresh/poll.
pub async fn inspector_stream(State(state): State<Arc<AppState>>) -> Response {
    let Some(hub) = &state.inspector else {
        return (StatusCode::NOT_FOUND, INSPECTOR_DISABLED_MESSAGE).into_response();
    };

    let mut rx = hub.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(exchange) => {
                    if let Ok(json) = serde_json::to_string(&exchange) {
                        yield Ok::<Event, std::convert::Infallible>(Event::default().data(json));
                    }
                }
                // A slow/absent client missed some events — that's fine for
                // a debug dashboard, just carry on with whatever comes next
                // rather than tearing down the connection.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
