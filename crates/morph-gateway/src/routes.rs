use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::pipeline::{self, PipelineOutcome};
use crate::state::AppState;

async fn dispatch(state: Arc<AppState>, protocol_id: &str, body: Bytes) -> Response {
    let Some(protocol) = state.protocols.get(protocol_id) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "protocol not registered").into_response();
    };
    let request_id = uuid::Uuid::new_v4().to_string();

    match pipeline::handle(state, protocol, request_id, body).await {
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

pub async fn openai_chat(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    dispatch(state, "openai_chat", body).await
}

pub async fn anthropic_messages(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    dispatch(state, "anthropic_messages", body).await
}

pub async fn ollama_chat(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    dispatch(state, "ollama", body).await
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
