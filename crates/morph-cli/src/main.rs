mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Morph — a transparent AI gateway. Point your existing client at
/// `http://localhost:8080` instead of your provider's real endpoint and
/// Morph takes care of the rest.
#[derive(Parser)]
#[command(name = "morph", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the config file. Created automatically on first run if it
    /// doesn't exist yet.
    #[arg(short, long, global = true, default_value = "morph.toml")]
    config: PathBuf,
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
    },
    /// List loaded WASM plugins.
    Plugins,
    /// Run a minimal built-in smoke test of the local pipeline (no network).
    Test,
    /// Time each built-in renderer against a representative fixture.
    Benchmark,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("morph=info".parse().unwrap()),
        )
        .init();

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
        Commands::Inspect { text, file } => commands::inspect::execute(text, file),
        Commands::Plugins => commands::plugins::execute(&cli.config),
        Commands::Test => commands::test::execute(),
        Commands::Benchmark => commands::benchmark::execute(),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
