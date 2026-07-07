mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Morph — a transparent AI gateway. Point your existing client at
/// `http://localhost:8080` instead of your provider's real endpoint and
/// Morph takes care of the rest.
///
/// `morph -- <command> [args...]` is a shortcut that starts the gateway,
/// points every common AI-CLI base-URL convention at it (Anthropic, OpenAI,
/// Ollama), runs `<command>` in the foreground, and exits when it does —
/// e.g. `morph -- claude` or `morph -- cursor`. Works with any tool; no
/// per-tool configuration needed.
#[derive(Parser)]
#[command(name = "morph", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the config file. Created automatically on first run if it
    /// doesn't exist yet.
    #[arg(short, long, global = true, default_value = "morph.toml")]
    config: PathBuf,

    /// Extra environment variable(s) to set on the command run via
    /// `morph -- <command>`, as `NAME=VALUE`. Repeatable. Use this for a
    /// tool whose base-URL env var isn't one of the common ones Morph
    /// already sets automatically.
    #[arg(long = "env", global = true)]
    extra_env: Vec<String>,

    /// The command to run once Morph is listening — everything after `--`.
    #[arg(last = true)]
    exec: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway (this is also what running `morph` with no
    /// subcommand does).
    Run,
    /// Create a starter morph.toml in the current directory.
    Init,
    /// Check your environment and config for common problems.
    Doctor,
    /// Print the effective, fully-resolved configuration.
    Config,
    /// List configured providers and what they can do.
    Providers,
    /// Render a Markdown/code/JSON/log/table file to an image, standalone
    /// (no server, no network call).
    Render {
        file: PathBuf,
        /// Where to write the rendered image. Defaults to `<file>.png`.
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long, default_value = "dark")]
        theme: String,
        #[arg(long, default_value = "png")]
        format: String,
    },
    /// Dry-run the detect → plan pipeline against a prompt, with no network
    /// call — shows what Morph *would* do and why.
    Inspect {
        /// Text to analyze. Reads from a file with `--file` instead if set.
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Actually render and save any image(s) Morph would attach, into
        /// this directory — using the exact same renderers (including
        /// plugins, if enabled) the live gateway calls. This is the way to
        /// *see* what a prompt turns into, not just read the plan for it.
        #[arg(long)]
        save_images: Option<PathBuf>,
    },
    /// List loaded WASM plugins.
    Plugins,
    /// Run a minimal built-in smoke test of the local pipeline (no network).
    Test,
    /// Time each built-in renderer against a representative fixture.
    Benchmark,
}

/// Parses a comma-separated `[logging].level` value into individual
/// `tracing_subscriber` directives — the same syntax `RUST_LOG` accepts.
/// Returns the first parse error's source text (for the warning printed at
/// the call site) rather than a full error type, since this is only ever
/// consumed as "did it work, and if not, what did the user write".
fn parse_directives(
    spec: &str,
) -> Result<Vec<tracing_subscriber::filter::Directive>, String> {
    spec.split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| d.parse().map_err(|_| d.to_string()))
        .collect()
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Under `morph -- <command>`, Morph's own request/startup logging would
    // otherwise interleave with the wrapped tool's own terminal output —
    // default to quiet there (still overridable via `[logging].level` in
    // config, or RUST_LOG). The config file may not exist yet on a first
    // run (e.g. before `morph init`/the first `morph run`), so a missing or
    // unreadable one just falls through to the built-in default below,
    // exactly like today.
    let fallback_directive = if cli.exec.is_empty() {
        "morph=info"
    } else {
        "morph=warn"
    };
    let configured_level = morph_config::load(&cli.config)
        .ok()
        .and_then(|c| c.logging.level);

    let mut filter = tracing_subscriber::EnvFilter::from_default_env();
    match configured_level.as_deref().map(parse_directives) {
        Some(Ok(directives)) => {
            for directive in directives {
                filter = filter.add_directive(directive);
            }
        }
        Some(Err(bad)) => {
            eprintln!(
                "warning: invalid [logging].level directive {bad:?} in {}, using the default instead",
                cli.config.display()
            );
            filter = filter.add_directive(fallback_directive.parse().unwrap());
        }
        None => {
            filter = filter.add_directive(fallback_directive.parse().unwrap());
        }
    }
    tracing_subscriber::fmt().with_env_filter(filter).init();

    if !cli.exec.is_empty() {
        if let Err(e) = commands::exec::execute(&cli.config, cli.exec, cli.extra_env).await {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    let result = match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => commands::run::execute(&cli.config).await,
        Commands::Init => commands::init::execute(&cli.config),
        Commands::Doctor => commands::doctor::execute(&cli.config).await,
        Commands::Config => commands::config::execute(&cli.config),
        Commands::Providers => commands::providers::execute(&cli.config),
        Commands::Render {
            file,
            out,
            theme,
            format,
        } => commands::render::execute(&file, out.as_deref(), &theme, &format),
        Commands::Inspect {
            text,
            file,
            save_images,
        } => commands::inspect::execute(&cli.config, text, file, save_images).await,
        Commands::Plugins => commands::plugins::execute(&cli.config),
        Commands::Test => commands::test::execute(),
        Commands::Benchmark => commands::benchmark::execute(),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
