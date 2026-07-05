use std::path::Path;

enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

fn print(status: &Status) {
    match status {
        Status::Ok(msg) => println!("  [ok]   {msg}"),
        Status::Warn(msg) => println!("  [warn] {msg}"),
        Status::Fail(msg) => println!("  [fail] {msg}"),
    }
}

pub async fn execute(config_path: &Path) -> anyhow::Result<()> {
    println!("Checking your Morph setup...\n");
    let mut checks = Vec::new();

    let config = match morph_config::load(config_path) {
        Ok(c) => {
            checks.push(Status::Ok(format!(
                "config file {} parses",
                config_path.display()
            )));
            c
        }
        Err(e) => {
            checks.push(Status::Fail(format!(
                "config file {} failed to load: {e} (run 'morph init' to create a fresh one)",
                config_path.display()
            )));
            for c in &checks {
                print(c);
            }
            return Ok(());
        }
    };

    if config.providers.is_empty() {
        checks.push(Status::Warn(
            "no providers configured — add a [providers.<name>] section".to_string(),
        ));
    }
    for (name, provider) in &config.providers {
        match provider.kind.as_str() {
            "openai" | "anthropic" => {
                if provider.passthrough_auth {
                    checks.push(Status::Ok(format!(
                        "provider \"{name}\" ({}) uses passthrough_auth — forwards whatever credential the client sends, no key needed here",
                        provider.kind
                    )));
                } else if provider.resolve_api_key().is_some() {
                    checks.push(Status::Ok(format!("provider \"{name}\" ({}) has an API key configured", provider.kind)));
                } else {
                    checks.push(Status::Warn(format!(
                        "provider \"{name}\" ({}) has no API key — fine for a local server (e.g. Ollama), otherwise requests will fail",
                        provider.kind
                    )));
                }
            }
            other => checks.push(Status::Fail(format!("provider \"{name}\" has unknown kind \"{other}\" (expected \"openai\" or \"anthropic\")"))),
        }
    }

    if !config.providers.contains_key(&config.default_provider) {
        checks.push(Status::Fail(format!(
            "default_provider = \"{}\" doesn't match any [providers.*] section",
            config.default_provider
        )));
    }

    match tokio::net::TcpListener::bind(&config.listen).await {
        Ok(_) => checks.push(Status::Ok(format!(
            "listen address {} is available",
            config.listen
        ))),
        Err(e) => checks.push(Status::Fail(format!("can't bind {}: {e}", config.listen))),
    }

    if config.plugins.enabled {
        let dir = std::path::Path::new(&config.plugins.dir);
        match morph_plugin_host::PluginRuntime::new() {
            Ok(runtime) => match morph_plugin_host::load_plugins_from_dir(dir, &runtime) {
                Ok(discovered) => checks.push(Status::Ok(format!(
                    "plugins directory {} loaded ({} renderer(s), {} classifier(s), {} transformer(s))",
                    dir.display(),
                    discovered.renderers.len(),
                    discovered.classifiers.len(),
                    discovered.transformers.len()
                ))),
                Err(e) => checks.push(Status::Warn(format!("plugins directory {} had a problem: {e}", dir.display()))),
            },
            Err(e) => checks.push(Status::Fail(format!("failed to initialize the WASM plugin runtime: {e}"))),
        }
    } else {
        checks.push(Status::Ok("plugins disabled".to_string()));
    }

    if config.inspector.enabled {
        checks.push(Status::Warn(format!(
            "inspector enabled at /_inspector — holds full prompt/response content (max {} exchanges) in memory, don't expose this instance publicly",
            config.inspector.max_events
        )));
    } else {
        checks.push(Status::Ok("inspector disabled".to_string()));
    }

    for c in &checks {
        print(c);
    }
    println!();
    let failures = checks
        .iter()
        .filter(|c| matches!(c, Status::Fail(_)))
        .count();
    if failures == 0 {
        println!("Everything looks good.");
    } else {
        println!("{failures} problem(s) found — see [fail] lines above.");
    }
    Ok(())
}
