use std::path::Path;

pub fn execute(config_path: &Path) -> anyhow::Result<()> {
    super::ensure_config_exists(config_path)?;
    let config = morph_config::load(config_path)?;

    if !config.plugins.enabled {
        println!(
            "Plugins are disabled (set [plugins] enabled = true in {} to turn them on).",
            config_path.display()
        );
        return Ok(());
    }

    let runtime = morph_plugin_host::PluginRuntime::new()?;
    let dir = Path::new(&config.plugins.dir);
    let discovered = morph_plugin_host::load_plugins_from_dir(dir, &runtime)
        .map_err(|e| anyhow::anyhow!("failed to load plugins from {}: {e}", dir.display()))?;

    if discovered.infos.is_empty() {
        println!("No plugins found in {}.", dir.display());
        return Ok(());
    }

    println!(
        "Loaded {} plugin(s) from {}:",
        discovered.infos.len(),
        dir.display()
    );
    for info in &discovered.infos {
        println!(
            "  {} v{} — kind={} abi={}{}",
            info.name,
            info.version,
            info.kind,
            info.abi_version,
            if info.supported_kinds.is_empty() {
                String::new()
            } else {
                format!(" supports=[{}]", info.supported_kinds.join(", "))
            }
        );
    }
    Ok(())
}
