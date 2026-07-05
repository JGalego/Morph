use std::path::Path;

pub fn execute(config_path: &Path) -> anyhow::Result<()> {
    super::ensure_config_exists(config_path)?;
    let config = morph_config::load(config_path)?;
    println!("{}", toml::to_string_pretty(&config)?);
    Ok(())
}
