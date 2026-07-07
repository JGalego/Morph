//! Backs `morph -- <command> [args...]`: start the gateway, point every
//! common AI-CLI base-URL convention at it, run `<command>` in the
//! foreground, and exit (taking the gateway down with it) when it does.
//! Deliberately tool-agnostic — no special-casing for any particular
//! client. `--env NAME=VALUE` covers a one-off literal value; `[exec]` in
//! `morph.toml` (`morph_config::ExecConfig`) covers a var name you want
//! pointed at Morph every time, the same as the built-in set below.

use std::path::Path;
use std::time::Duration;

use morph_config::Config;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, Instant};

/// Every common AI-client base-URL env var convention, all pointed at Morph
/// at once — unused ones are simply ignored by tools that don't look for
/// them, so this needs no per-tool knowledge. `OLLAMA_HOST` is a bare
/// `host:port`, not a URL, so it's set separately in `execute`. New
/// conventions (e.g. a CLI's own `*_BASE_URL` var) just get appended here.
const BASE_URL_ENV_VARS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "COPILOT_PROVIDER_BASE_URL",
];

/// Paired with `BASE_URL_ENV_VARS`: credential env vars a client might read
/// directly, which must be cleared so a stray one left over from another
/// shell session can't silently bypass Morph (or shadow `passthrough_auth`)
/// — see docs/PROVIDERS.md.
const CREDENTIAL_ENV_VARS_TO_CLEAR: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
];

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
    // Taken before `spawn_server` moves `config` away.
    let extra_base_url_vars = config.exec.extra_base_url_env_vars.clone();
    let extra_clear_vars = config.exec.extra_clear_env_vars.clone();

    spawn_server(config);
    wait_until_listening(&host_port).await?;

    let mut command = windows_aware_command(&program);
    command.args(&args).env("OLLAMA_HOST", &ollama_host);
    let base_url_vars = BASE_URL_ENV_VARS
        .iter()
        .copied()
        .chain(extra_base_url_vars.iter().map(String::as_str));
    for var in base_url_vars {
        command.env(var, &base_url);
    }
    // A stray credential left over from another shell session must not
    // silently bypass Morph (or shadow `passthrough_auth`) — see
    // docs/PROVIDERS.md.
    let clear_vars = CREDENTIAL_ENV_VARS_TO_CLEAR
        .iter()
        .copied()
        .chain(extra_clear_vars.iter().map(String::as_str));
    for var in clear_vars {
        command.env_remove(var);
    }

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
