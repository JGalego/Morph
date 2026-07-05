use std::path::Path;

use crate::error::{ConfigError, Result};
use crate::schema::Config;

pub fn load(path: impl AsRef<Path>) -> Result<Config> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse(&text, path)
}

pub fn parse(text: &str, path: impl AsRef<Path>) -> Result<Config> {
    toml::from_str(text).map_err(|source| ConfigError::Parse {
        path: path.as_ref().display().to_string(),
        source,
    })
}

/// Writes a fully-commented, ready-to-edit default config to `path`. Used by
/// `morph init`.
pub fn write_default(path: impl AsRef<Path>) -> Result<()> {
    let config = Config::default();
    let body = toml::to_string_pretty(&config)?;
    std::fs::write(&path, body).map_err(|source| ConfigError::Read {
        path: path.as_ref().display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_spec_example() {
        let toml = r#"
            listen = "0.0.0.0:8080"
            mode = "auto"
            theme = "dark"
            cache = true
            stream = true
            metrics = true
        "#;
        let cfg = parse(toml, "test.toml").unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:8080");
        assert_eq!(cfg.mode, "auto");
        assert!(cfg.cache);
        // providers table absent from this minimal example -> falls back to
        // the built-in default (empty map), not the struct-level Default::default().
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn round_trips_default_config() {
        let cfg = Config::default();
        let body = toml::to_string_pretty(&cfg).unwrap();
        let parsed = parse(&body, "roundtrip.toml").unwrap();
        assert_eq!(parsed.listen, cfg.listen);
        assert_eq!(parsed.providers.len(), cfg.providers.len());
    }

    #[test]
    fn resolves_api_key_from_env() {
        let mut cfg = Config::default();
        let provider = cfg.providers.get_mut("openai").unwrap();
        provider.api_key = None;
        provider.api_key_env = Some("MORPH_TEST_KEY_XYZ".to_string());
        std::env::set_var("MORPH_TEST_KEY_XYZ", "secret123");
        assert_eq!(provider.resolve_api_key(), Some("secret123".to_string()));
        std::env::remove_var("MORPH_TEST_KEY_XYZ");
    }
}
