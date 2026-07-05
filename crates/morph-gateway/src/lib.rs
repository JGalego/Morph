//! Morph's HTTP gateway: an axum server that routes by path to the
//! matching `ProtocolAdapter`, runs the shared pipeline (see `pipeline`),
//! and forwards to whichever `ProviderAdapter` the (hot-reloadable) config
//! currently names as `default_provider`.
//!
//! Structural pieces — which providers exist, which plugins are loaded,
//! which middleware/transformers run — are assembled once at startup
//! (`build::build_app_state`) and fixed for the process's lifetime; adding
//! a provider or plugin needs a restart. Operational knobs consulted from
//! the live `watch::Receiver<Config>` on every request — `default_provider`,
//! `mode`, `theme`, render thresholds, and the `cache`/`auth`/`rate_limit`
//! enabled flags — take effect immediately on a config file edit, no
//! restart required. See `morph-config::ConfigWatcher`.

mod auth;
mod build;
mod classifier;
mod pipeline;
mod representation;
mod routes;
mod state;

use std::path::Path;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::{middleware, Extension, Router};
use metrics_exporter_prometheus::PrometheusBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

pub use build::build_app_state;
pub use state::AppState;

/// Builds the axum `Router` for a given, already-assembled `AppState`.
/// Split out from `serve` so tests (and `morph inspect`/`morph doctor`) can
/// exercise the exact same routing/pipeline without binding a real socket.
pub fn router(
    state: Arc<AppState>,
    prometheus: metrics_exporter_prometheus::PrometheusHandle,
) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(routes::openai_chat))
        .route("/v1/messages", post(routes::anthropic_messages))
        .route("/api/chat", post(routes::ollama_chat))
        .route("/v1/models", get(routes::list_models))
        .route("/health", get(routes::health))
        .route("/metrics", get(routes::metrics))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::rate_limit,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .layer(Extension(prometheus))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Loads `config_path`, assembles the pipeline, and serves until the
/// process receives a shutdown signal (Ctrl-C or SIGTERM). Installs the
/// process-global Prometheus recorder — call this at most once per process.
pub async fn serve(config_path: &Path) -> anyhow::Result<()> {
    let watcher = morph_config::ConfigWatcher::spawn(config_path)?;
    let config = watcher.current();
    let listen = config.listen.clone();

    let recorder = PrometheusBuilder::new().build_recorder();
    let prometheus_handle = recorder.handle();
    metrics::set_global_recorder(recorder)
        .map_err(|e| anyhow::anyhow!("failed to install metrics recorder: {e}"))?;

    let state = Arc::new(build_app_state(&config, watcher.receiver())?);
    let app = router(state, prometheus_handle);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(%listen, "morph listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
}
