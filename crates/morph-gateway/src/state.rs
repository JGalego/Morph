use std::sync::Arc;
use std::time::Instant;

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use morph_config::Config;
use morph_core::registry::Registry;
use morph_core::traits::{
    Classifier, Middleware, ProtocolAdapter, ProviderAdapter, Renderer, RepresentationPlanner,
    Transformer,
};
use morph_middleware::{InMemoryStatsSink, ResponseCache};
use morph_plugin_host::PluginRuntime;
use tokio::sync::watch;

use crate::inspector::InspectorHub;

/// Everything a request handler needs, assembled once at startup (and
/// re-assembled on config hot-reload) and shared behind an `Arc` across
/// every axum handler.
pub struct AppState {
    pub config: watch::Receiver<Config>,
    pub protocols: Registry<dyn ProtocolAdapter>,
    pub providers: Registry<dyn ProviderAdapter>,
    pub renderers: Vec<Arc<dyn Renderer>>,
    pub classifier: Arc<dyn Classifier>,
    pub planner: Arc<dyn RepresentationPlanner>,
    /// Request-side transformers (redaction, plugin transformers). Applied
    /// in order after protocol parsing, before representation planning.
    pub request_transformers: Vec<Arc<dyn Transformer>>,
    /// Response-side transformers. Only applied to buffered (non-streaming)
    /// responses — see the module docs on `pipeline` for why streaming
    /// responses skip this stage.
    pub response_transformers: Vec<Arc<dyn Transformer>>,
    pub middlewares: Vec<Arc<dyn Middleware>>,
    pub stats_sink: Arc<InMemoryStatsSink>,
    pub cache: Option<Arc<ResponseCache>>,
    /// Kept alive for as long as `AppState` is, since `PluginRuntime` owns
    /// the wasmtime `Engine`/`Linker` every loaded plugin's calls run
    /// against.
    pub _plugin_runtime: Option<PluginRuntime>,
    pub plugin_infos: Vec<morph_plugin_abi::exports::morph::plugin::plugin::PluginInfo>,
    pub rate_limiter: Option<Arc<DefaultDirectRateLimiter>>,
    pub started_at: Instant,
    /// `None` unless `[inspector] enabled = true` — see `inspector` module
    /// docs for why that's the point at which capture overhead exists at all.
    pub inspector: Option<Arc<InspectorHub>>,
}

impl AppState {
    pub fn build_rate_limiter(requests_per_minute: u32) -> Option<Arc<DefaultDirectRateLimiter>> {
        let quota = Quota::per_minute(std::num::NonZeroU32::new(requests_per_minute.max(1))?);
        Some(Arc::new(RateLimiter::direct(quota)))
    }
}
