use std::path::Path;

pub fn execute(config_path: &Path) -> anyhow::Result<()> {
    super::ensure_config_exists(config_path)?;
    let config = morph_config::load(config_path)?;

    println!("Built-in provider kinds:");
    println!("  openai     generic OpenAI-wire adapter — also covers Azure OpenAI, Ollama,");
    println!("             vLLM, LM Studio, OpenRouter, Together, Groq, Cerebras, Mistral,");
    println!(
        "             DeepSeek, xAI (anything speaking the OpenAI chat/completions wire format)"
    );
    println!("  anthropic  Anthropic Messages API");
    println!();

    if config.providers.is_empty() {
        println!("No providers configured in {}.", config_path.display());
        return Ok(());
    }

    println!("Configured providers:");
    for (name, provider) in &config.providers {
        let default_marker = if *name == config.default_provider {
            " (default)"
        } else {
            ""
        };
        let key_status = if provider.resolve_api_key().is_some() {
            "key set"
        } else {
            "no key"
        };
        println!(
            "  {name}{default_marker}: kind={} base_url={} [{key_status}]",
            provider.kind, provider.base_url
        );
    }
    Ok(())
}
