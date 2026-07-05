pub mod benchmark;
pub mod config;
pub mod doctor;
pub mod init;
pub mod inspect;
pub mod plugins;
pub mod providers;
pub mod render;
pub mod run;
pub mod test;

/// Ensures a config file exists at `path`, creating a starter one with a
/// friendly notice if it doesn't. This is the single biggest lever for
/// "easy to install & setup for people with a limited grasp of a
/// terminal": running bare `morph` in an empty directory should just work,
/// not fail with a parse error about a file that was never explained.
pub fn ensure_config_exists(path: &std::path::Path) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    morph_config::write_default(path)?;
    eprintln!(
        "No config found at {} — created one with defaults.",
        path.display()
    );
    eprintln!("Edit it to add your API key, or set OPENAI_API_KEY in your environment and rerun.");
    Ok(())
}
