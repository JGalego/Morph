//! `morph benchmark`: a minimal local benchmark of the rendering engine —
//! not a full HTTP load-testing tool (that's out of scope for v1; see the
//! project roadmap). Useful for a quick "did I just regress render
//! performance" check while developing a new renderer or plugin.

use std::time::Instant;

use morph_core::content::{ContentKind, DetectedContent};
use morph_core::representation::{RasterFormat, RenderOptions, Theme};

const ITERATIONS: u32 = 20;

fn fixture_for(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Markdown => "# Title\n\nSome paragraph text.\n\n- one\n- two\n- three\n",
        ContentKind::Code => {
            "fn fib(n: u64) -> u64 {\n    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\n"
        }
        ContentKind::Json => {
            "{\"users\": [{\"id\": 1, \"name\": \"a\"}, {\"id\": 2, \"name\": \"b\"}]}"
        }
        ContentKind::Table => "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n",
        ContentKind::TerminalLog => "\x1b[31mERROR\x1b[0m something failed\nINFO ok\n",
        _ => "plain text",
    }
}

pub fn execute() -> anyhow::Result<()> {
    let renderers = morph_render::default_renderers();
    let opts = RenderOptions {
        theme: Theme::Dark,
        max_width_px: 1200,
        scale: 1.0,
        format: RasterFormat::Png,
    };

    println!("Rendering each built-in renderer {ITERATIONS} times (PNG, 1200px, dark theme):\n");
    for renderer in &renderers {
        let kind = [
            ContentKind::Markdown,
            ContentKind::Code,
            ContentKind::Json,
            ContentKind::Table,
            ContentKind::TerminalLog,
        ]
        .into_iter()
        .find(|k| renderer.supports(*k));
        let Some(kind) = kind else { continue };

        let content = DetectedContent {
            kind,
            ..DetectedContent::plain(fixture_for(kind))
        };
        let start = Instant::now();
        let mut bytes = 0;
        for _ in 0..ITERATIONS {
            match renderer.render(&content, &opts) {
                Ok(asset) => bytes = asset.bytes.len(),
                Err(e) => {
                    println!("  {:<10} FAILED: {e}", renderer.name());
                    continue;
                }
            }
        }
        let elapsed = start.elapsed();
        println!(
            "  {:<10} {:>8.2} ms/iter   ({bytes} bytes/output)",
            renderer.name(),
            elapsed.as_secs_f64() * 1000.0 / ITERATIONS as f64
        );
    }
    Ok(())
}
