//! End-to-end tests that exercise the real axum `Router` (via
//! `tower::ServiceExt::oneshot`, no real TCP socket needed) against a
//! `wiremock` stand-in for the upstream provider. These are what actually
//! prove "any client protocol in, any provider out" — unit tests within
//! each crate already cover the pieces in isolation.

use std::sync::Arc;

use http_body_util::BodyExt;
use metrics_exporter_prometheus::PrometheusBuilder;
use morph_config::{Config, ProviderConfig};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn base_config(kind: &str, base_url: &str) -> Config {
    let mut config = Config::default();
    config.providers.clear();
    config.providers.insert(
        "test".to_string(),
        ProviderConfig {
            kind: kind.to_string(),
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            api_key_env: None,
            default_model: None,
        },
    );
    config.default_provider = "test".to_string();
    config.plugins.enabled = false;
    config
}

fn build_router(config: Config) -> axum::Router {
    let (_tx, rx) = tokio::sync::watch::channel(config.clone());
    let state =
        Arc::new(morph_gateway::build_app_state(&config, rx).expect("failed to build app state"));
    let prometheus = PrometheusBuilder::new().build_recorder().handle();
    morph_gateway::router(state, prometheus)
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "response body is not valid JSON ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

const OPENAI_SSE_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello from Morph!\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
    "data: [DONE]\n\n",
);

const ANTHROPIC_SSE_BODY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-3-5-sonnet-20241022\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from Morph!\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

#[tokio::test]
async fn openai_ingress_to_openai_provider_buffered() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_SSE_BODY.as_bytes(), "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let router = build_router(base_config("openai", &mock_server.uri()));

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}], "stream": false}).to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;
    let text = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("Hello from Morph!"),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn anthropic_ingress_to_anthropic_provider_buffered() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(ANTHROPIC_SSE_BODY.as_bytes(), "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let router = build_router(base_config("anthropic", &mock_server.uri()));

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"model": "claude-3-5-sonnet-20241022", "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}], "stream": false}).to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;
    let text = body["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("Hello from Morph!"),
        "unexpected body: {body}"
    );
}

/// The core "any client, any provider" claim: an Ollama-shaped client
/// request, served by an OpenAI-wire provider underneath, through the same
/// running gateway.
#[tokio::test]
async fn ollama_ingress_to_openai_provider_buffered() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_SSE_BODY.as_bytes(), "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let router = build_router(base_config("openai", &mock_server.uri()));

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/chat")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}], "stream": false}).to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;
    assert_eq!(body["message"]["content"], "Hello from Morph!");
    assert_eq!(body["done"], true);
}

#[tokio::test]
async fn streaming_openai_request_returns_sse_content_type_and_forwards_chunks() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_SSE_BODY.as_bytes(), "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let router = build_router(base_config("openai", &mock_server.uri()));

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}], "stream": true}).to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("Hello from Morph!"),
        "unexpected stream body: {text}"
    );
    assert!(text.contains("data:"), "expected SSE-framed chunks: {text}");
}

#[tokio::test]
async fn health_endpoint_reports_configured_provider() {
    let mock_server = MockServer::start().await;
    let router = build_router(base_config("openai", &mock_server.uri()));

    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["providers"][0], "test");
}

#[tokio::test]
async fn unknown_provider_kind_is_rejected_at_startup() {
    let mock_server = MockServer::start().await;
    let mut config = base_config("openai", &mock_server.uri());
    config.providers.get_mut("test").unwrap().kind = "not-a-real-provider".to_string();

    let (_tx, rx) = tokio::sync::watch::channel(config.clone());
    let result = morph_gateway::build_app_state(&config, rx);
    assert!(
        result.is_err(),
        "an unknown provider kind should fail fast, not silently no-op"
    );
}
