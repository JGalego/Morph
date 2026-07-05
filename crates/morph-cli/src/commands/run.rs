use std::path::Path;

pub async fn execute(config_path: &Path) -> anyhow::Result<()> {
    super::ensure_config_exists(config_path)?;
    println!("Starting Morph — press Ctrl-C to stop.");
    morph_gateway::serve(config_path).await
}
