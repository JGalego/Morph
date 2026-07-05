//! Backs `morph -- <command> [args...]`: start the gateway, point every
//! common AI-CLI base-URL convention at it, run `<command>` in the
//! foreground, and exit (taking the gateway down with it) when it does.
//! Deliberately tool-agnostic — no special-casing for any particular
//! client. `--env NAME=VALUE` covers anything not in the built-in set.

use std::path::Path;
use std::time::Duration;

use morph_config::Config;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, Instant};

/// `listen` is a bind address (`0.0.0.0:8080` is valid to bind, not to
/// connect to) — this is the address a client on the same machine should
/// actually use.
fn connectable_host_port(listen: &str) -> String {
    match listen.strip_prefix("0.0.0.0:") {
        Some(port) => format!("127.0.0.1:{port}"),
        None => listen.to_string(),
    }
}

/// `Command::new(program).spawn()` calls `CreateProcess` directly, which
/// only auto-resolves `.exe`/`.com` — never the `.cmd`/`.bat` shims npm (and
/// most other JS tooling) installs on Windows as the real entry point.
/// Routing through `cmd /C` gets `cmd.exe`'s own PATHEXT-aware search and
/// script dispatch instead, the same workaround Node's `child_process.spawn`
/// applies internally for this exact case.
#[cfg(windows)]
fn windows_aware_command(program: &str) -> Command {
    let mut command = Command::new("cmd");
    command.arg("/C").arg(program);
    command
}

#[cfg(not(windows))]
fn windows_aware_command(program: &str) -> Command {
    Command::new(program)
}

async fn wait_until_listening(host_port: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(host_port).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for morph to start listening on {host_port}");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn execute(
    config_path: &Path,
    mut args: Vec<String>,
    extra_env: Vec<String>,
) -> anyhow::Result<()> {
    if args.is_empty() {
        anyhow::bail!("usage: morph -- <command> [args...]");
    }
    let program = args.remove(0);

    super::ensure_config_exists(config_path)?;
    let config = morph_config::load(config_path)?;
    println!(
        "Starting Morph (provider \"{}\", mode \"{}\") for '{program}'...",
        config.default_provider, config.mode
    );

    let host_port = connectable_host_port(&config.listen);
    let base_url = format!("http://{host_port}");
    let ollama_host = host_port.clone();

    spawn_server(config);
    wait_until_listening(&host_port).await?;

    let mut command = windows_aware_command(&program);
    command
        .args(&args)
        // Every common AI-client base-URL convention, all pointed at
        // Morph at once — unused ones are simply ignored by tools that
        // don't look for them, so this needs no per-tool knowledge.
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env("OPENAI_BASE_URL", &base_url)
        .env("OPENAI_API_BASE", &base_url)
        .env("OLLAMA_HOST", &ollama_host)
        // A stray credential left over from another shell session must
        // not silently bypass Morph (or shadow `passthrough_auth`) —
        // see docs/PROVIDERS.md.
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("OPENAI_API_KEY");

    for pair in &extra_env {
        let Some((key, value)) = pair.split_once('=') else {
            anyhow::bail!("--env expects KEY=VALUE, got \"{pair}\"");
        };
        command.env(key, value);
    }

    let status = command.status().await;
    let status = match status {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("'{program}' was not found on your PATH")
        }
        Err(e) => anyhow::bail!("failed to launch '{program}': {e}"),
    };

    println!("'{program}' exited — stopping Morph.");
    std::process::exit(status.code().unwrap_or(1));
}

fn spawn_server(config: Config) {
    tokio::spawn(async move {
        if let Err(e) = morph_gateway::serve_with_config(config).await {
            eprintln!("morph server error: {e:#}");
        }
    });
}
