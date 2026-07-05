use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use morph_config::Config;
use morph_core::registry::Registry;
use morph_core::traits::{Middleware, ProtocolAdapter, ProviderAdapter, Renderer, Transformer};
use morph_detect::{DefaultClassifier, DefaultPlanner};
use morph_middleware::{
    InMemoryStatsSink, LoggingMiddleware, MetricsMiddleware, RedactionTransformer, ResponseCache,
};
use morph_plugin_host::PluginRuntime;
use morph_protocols::{AnthropicMessagesProtocol, OllamaProtocol, OpenAiChatProtocol};
use morph_providers::{AnthropicProvider, OpenAiProvider};
use tokio::sync::watch;

use crate::classifier::CompositeClassifier;
use crate::inspector::InspectorHub;
use crate::state::AppState;

/// Assembles an `AppState` from a `Config`. Called once at startup and
/// again on every hot-reload (see `morph-cli`'s `run` command), so it must
/// be side-effect-free beyond loading plugin files from disk — no global
/// mutable state to reset, nothing left over from a previous build.
pub fn build_app_state(
    config: &Config,
    config_rx: watch::Receiver<Config>,
) -> anyhow::Result<AppState> {
    let protocols: Registry<dyn ProtocolAdapter> = Registry::new();
    protocols.register("openai_chat", Arc::new(OpenAiChatProtocol::new()));
    protocols.register(
        "anthropic_messages",
        Arc::new(AnthropicMessagesProtocol::new()),
    );
    protocols.register("ollama", Arc::new(OllamaProtocol::new()));

    let providers: Registry<dyn ProviderAdapter> = Registry::new();
    for (name, provider_cfg) in &config.providers {
        let api_key = provider_cfg.resolve_api_key();
        let adapter: Arc<dyn ProviderAdapter> = match provider_cfg.kind.as_str() {
            "openai" => Arc::new(OpenAiProvider::new(
                name.clone(),
                provider_cfg.base_url.clone(),
                api_key,
                provider_cfg.passthrough_auth,
            )),
            "anthropic" => Arc::new(AnthropicProvider::new(
                name.clone(),
                provider_cfg.base_url.clone(),
                api_key,
                provider_cfg.passthrough_auth,
            )),
            other => {
                anyhow::bail!(
                    "provider \"{name}\" declares unknown kind \"{other}\" (expected \"openai\" or \"anthropic\")"
                );
            }
        };
        providers.register(name.clone(), adapter);
    }

    let mut renderers: Vec<Arc<dyn Renderer>> = morph_render::default_renderers();
    let mut classifiers: Vec<Arc<dyn morph_core::traits::Classifier>> =
        vec![Arc::new(DefaultClassifier::new())];
    let mut plugin_transformers: Vec<Arc<dyn Transformer>> = Vec::new();
    let mut plugin_infos = Vec::new();
    let mut plugin_runtime = None;

    if config.plugins.enabled {
        let runtime = PluginRuntime::new()?;
        let discovered =
            morph_plugin_host::load_plugins_from_dir(Path::new(&config.plugins.dir), &runtime)
                .map_err(|e| anyhow::anyhow!("failed to load plugins: {e}"))?;
        renderers.extend(discovered.renderers);
        classifiers.extend(discovered.classifiers);
        plugin_transformers = discovered.transformers;
        plugin_infos = discovered.infos;
        plugin_runtime = Some(runtime);
    }

    // The same `Transformer` instance (native `RedactionTransformer`, or a
    // plugin implementing both methods) is used on both the request and
    // response side — each list just controls which pipeline stage calls
    // into it, per `Transformer`'s own request/response methods.
    let request_transformers: Vec<Arc<dyn Transformer>> =
        std::iter::once(Arc::new(RedactionTransformer) as Arc<dyn Transformer>)
            .chain(plugin_transformers.iter().cloned())
            .collect();
    let response_transformers: Vec<Arc<dyn Transformer>> =
        std::iter::once(Arc::new(RedactionTransformer) as Arc<dyn Transformer>)
            .chain(plugin_transformers.iter().cloned())
            .collect();

    let mut middlewares: Vec<Arc<dyn Middleware>> =
        vec![Arc::new(LoggingMiddleware::new(config.logging.log_prompts))];
    if config.metrics {
        middlewares.push(Arc::new(MetricsMiddleware));
    }

    let cache = if config.cache {
        Some(Arc::new(ResponseCache::new(1000, Duration::from_secs(300))))
    } else {
        None
    };

    let rate_limiter = if config.rate_limit.enabled {
        AppState::build_rate_limiter(config.rate_limit.requests_per_minute)
    } else {
        None
    };

    let inspector = config
        .inspector
        .enabled
        .then(|| Arc::new(InspectorHub::new(config.inspector.max_events)));

    Ok(AppState {
        config: config_rx,
        protocols,
        providers,
        renderers,
        classifier: Arc::new(CompositeClassifier::new(classifiers)),
        planner: Arc::new(DefaultPlanner::new()),
        request_transformers,
        response_transformers,
        middlewares,
        stats_sink: Arc::new(InMemoryStatsSink::new()),
        cache,
        _plugin_runtime: plugin_runtime,
        plugin_infos,
        rate_limiter,
        started_at: Instant::now(),
        inspector,
    })
}
