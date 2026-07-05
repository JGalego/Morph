use std::path::Path;

pub fn execute(config_path: &Path) -> anyhow::Result<()> {
    if config_path.exists() {
        println!(
            "{} already exists — leaving it alone.",
            config_path.display()
        );
        println!("Delete it first if you want a fresh default config.");
        return Ok(());
    }
    morph_config::write_default(config_path)?;
    println!("Created {}.", config_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Set an API key, e.g.:  export OPENAI_API_KEY=sk-...");
    println!("  2. Start Morph:           morph");
    println!("  3. Point your AI client's API base URL at http://localhost:8080");
    Ok(())
}
