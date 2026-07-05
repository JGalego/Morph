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
use wiremock::matchers::{header, method, path};
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
            passthrough_auth: false,
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

/// The end-to-end proof for subscription-auth support (no Anthropic API
/// key configured at all): a raw HTTP request carrying the client's own
/// `authorization`/`anthropic-beta` headers, through the real router, must
/// reach the (mocked) upstream with those exact headers — and Morph's own
/// (absent) configured key must play no part.
#[tokio::test]
async fn passthrough_auth_forwards_the_clients_own_credential_end_to_end() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .and(header("authorization", "Bearer client-oauth-token"))
        .and(header("anthropic-beta", "oauth-2026-01-01"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-3-5-sonnet\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ),
            "text/event-stream",
        ))
        .mount(&mock_server)
        .await;

    let mut config = base_config("anthropic", &mock_server.uri());
    let provider = config.providers.get_mut("test").unwrap();
    provider.api_key = None;
    provider.passthrough_auth = true;
    let router = build_router(config);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("authorization", "Bearer client-oauth-token")
        .header("anthropic-beta", "oauth-2026-01-01")
        .body(axum::body::Body::from(
            json!({"model": "claude-3-5-sonnet", "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}], "stream": false}).to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "a 200 here proves wiremock matched the forwarded headers"
    );
    let body = body_json(response).await;
    assert_eq!(body["content"][0]["text"], "hi");
}

#[tokio::test]
async fn inspector_is_a_404_when_disabled() {
    let mock_server = MockServer::start().await;
    let router = build_router(base_config("openai", &mock_server.uri()));

    for uri in ["/_inspector", "/_inspector/api/events"] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            404,
            "expected {uri} to 404 when the inspector is disabled"
        );
    }
}

/// The end-to-end proof for the whole feature: with the inspector and
/// `force_image_only` both enabled, a plain-text prompt sent through the
/// real router must show up at `/_inspector/api/events` with `received`
/// holding the original text and `sent` holding a rendered image in its
/// place — not just "the pipeline ran", but "the dashboard would show
/// exactly what changed".
#[tokio::test]
async fn inspector_captures_the_text_to_image_transformation() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OPENAI_SSE_BODY.as_bytes(), "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let mut config = base_config("openai", &mock_server.uri());
    config.mode = "force_image_only".to_string();
    config.inspector.enabled = true;
    let router = build_router(config);

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hello there, how are you?"}], "stream": false}).to_string(),
        ))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);

    let events_request = axum::http::Request::builder()
        .method("GET")
        .uri("/_inspector/api/events")
        .body(axum::body::Body::empty())
        .unwrap();
    let events_response = router.oneshot(events_request).await.unwrap();
    assert_eq!(events_response.status(), 200);
    let events = body_json(events_response).await;

    assert_eq!(events.as_array().unwrap().len(), 1);
    let exchange = &events[0];
    assert_eq!(exchange["protocol"], "openai_chat");
    assert_eq!(exchange["provider"], "test");
    assert_eq!(exchange["cached"], false);
    assert!(exchange["error"].is_null());

    // Received: the client's original plain text, untouched.
    let received_blocks = &exchange["received"]["messages"][0]["content"];
    assert_eq!(received_blocks[0]["type"], "text");
    assert_eq!(received_blocks[0]["text"], "hello there, how are you?");

    // Sent: the same message now carries a Morph-rendered image instead
    // (force_image_only replaces plain text outright).
    let sent_blocks = &exchange["sent"]["messages"][0]["content"];
    let image_block = sent_blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "image")
        .expect("expected an image block in the sent request");
    assert_eq!(image_block["rendered_by_morph"], true);
    assert_eq!(image_block["source"]["kind"], "base64");
    assert!(!image_block["source"]["data"].as_str().unwrap().is_empty());

    // Response: what the mocked provider actually said.
    assert_eq!(
        exchange["response"]["content"][0]["text"],
        "Hello from Morph!"
    );
}
