use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_true() -> bool {
    true
}

/// Root of `morph.toml`. Field names and the top-level flat shape match the
/// project's spec example (`listen`, `mode`, `theme`, `cache`, `stream`,
/// `metrics`) exactly; the `[providers.*]`/`[auth]`/`[rate_limit]`/
/// `[render]`/`[plugins]` tables extend it with what a multi-provider,
/// multi-protocol gateway needs beyond that minimal example.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,

    /// "auto" | "force_text" | "force_hybrid" | "force_image_only" — see
    /// `morph_core::PlannerMode`.
    #[serde(default = "default_mode")]
    pub mode: String,

    /// Key into `providers` used when a request doesn't otherwise specify one.
    #[serde(default = "default_provider_name")]
    pub default_provider: String,

    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_true")]
    pub cache: bool,

    #[serde(default = "default_true")]
    pub stream: bool,

    #[serde(default = "default_true")]
    pub metrics: bool,

    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    #[serde(default)]
    pub auth: AuthConfig,

    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    #[serde(default)]
    pub render: RenderConfig,

    #[serde(default)]
    pub plugins: PluginsConfig,

    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(default)]
    pub inspector: InspectorConfig,
}

fn default_mode() -> String {
    "auto".to_string()
}

fn default_provider_name() -> String {
    "openai".to_string()
}

fn default_theme() -> String {
    "dark".to_string()
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                kind: "openai".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: None,
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                default_model: None,
                passthrough_auth: false,
            },
        );

        Config {
            listen: default_listen(),
            mode: default_mode(),
            default_provider: default_provider_name(),
            theme: default_theme(),
            cache: true,
            stream: true,
            metrics: true,
            providers,
            auth: AuthConfig::default(),
            rate_limit: RateLimitConfig::default(),
            render: RenderConfig::default(),
            plugins: PluginsConfig::default(),
            logging: LoggingConfig::default(),
            inspector: InspectorConfig::default(),
        }
    }
}

/// One named backend. `kind = "openai"` is the generic OpenAI-wire adapter —
/// pointing its `base_url` at Azure OpenAI, Ollama, vLLM, LM Studio,
/// OpenRouter, Together, Groq, Cerebras, Mistral, DeepSeek, or xAI is what
/// makes all of those work without a dedicated adapter each.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub kind: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// When set, `api_key`/`api_key_env` are ignored and every header from
    /// the client's original request to Morph is forwarded verbatim on the
    /// upstream call instead — Morph never needs its own credential. This
    /// is what lets an OAuth-backed subscription login (e.g. Claude Code
    /// signed in via claude.ai, not an API key) work through Morph: Morph
    /// doesn't need to know which header carries the credential, it just
    /// replays whatever the client already sent.
    #[serde(default)]
    pub passthrough_auth: bool,
}

impl ProviderConfig {
    /// Resolves the API key from either the inline value or the named
    /// environment variable, inline taking precedence.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.api_key.clone().or_else(|| {
            self.api_key_env
                .as_ref()
                .and_then(|var| std::env::var(var).ok())
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub enabled: bool,
    pub api_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub requests_per_minute: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            enabled: false,
            requests_per_minute: 600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    pub min_tokens_for_rendering: usize,
    pub allow_code_as_image: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            min_tokens_for_rendering: 120,
            allow_code_as_image: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub enabled: bool,
    pub dir: String,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        PluginsConfig {
            enabled: false,
            dir: "./plugins".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Never log prompt/response content by default — see project security goals.
    pub log_prompts: bool,
    pub json: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            log_prompts: false,
            json: true,
        }
    }
}

/// The live "what did Morph get, what did it send" web dashboard, served at
/// `/_inspector` on the main gateway port. Off by default: this holds full
/// prompt/response content in memory — including any rendered images — so
/// enabling it on an instance reachable by anyone other than you is a real
/// exposure, not just a debug convenience. See docs/ARCHITECTURE.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectorConfig {
    pub enabled: bool,
    /// How many recent exchanges to keep in memory (oldest dropped first).
    pub max_events: usize,
}

impl Default for InspectorConfig {
    fn default() -> Self {
        InspectorConfig {
            enabled: false,
            max_events: 50,
        }
    }
}
