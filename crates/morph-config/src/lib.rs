//! TOML configuration schema and hot-reload for Morph. A single
//! `morph.toml` drives the whole gateway; editing it while `morph` is
//! running takes effect without a restart via [`watch::ConfigWatcher`].

pub mod error;
pub mod load;
pub mod schema;
pub mod watch;

pub use error::{ConfigError, Result};
pub use load::{load, parse, write_default};
pub use schema::{
    AuthConfig, Config, LoggingConfig, PluginsConfig, ProviderConfig, RateLimitConfig, RenderConfig,
};
pub use watch::ConfigWatcher;
