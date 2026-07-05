use std::path::{Path, PathBuf};

use morph_core::content::DetectedContent;
use morph_core::representation::{RasterFormat, RenderOptions, Theme};
use morph_core::traits::Classifier;

fn guess_language_from_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" => "shell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        _ => return None,
    };
    Some(lang.to_string())
}

pub fn execute(file: &Path, out: Option<&Path>, theme: &str, format: &str) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file.display()))?;

    let classifier = morph_detect::DefaultClassifier::new();
    let candidates = classifier.classify(&raw);
    let (kind, confidence) = candidates
        .first()
        .copied()
        .unwrap_or((morph_core::content::ContentKind::PlainText, 1.0));
    println!("Detected: {kind:?} (confidence {confidence:.2})");

    let mut content = DetectedContent::plain(raw);
    content.kind = kind;
    if kind == morph_core::content::ContentKind::Code {
        content.language = guess_language_from_extension(file);
    }

    let renderers = morph_render::default_renderers();
    let Some(renderer) = renderers.iter().find(|r| r.supports(kind)) else {
        anyhow::bail!(
            "no built-in renderer supports {kind:?} content yet (supported: Markdown, Code, Json, Table, TerminalLog, StackTrace)"
        );
    };

    let raster_format = if format.eq_ignore_ascii_case("svg") {
        RasterFormat::Svg
    } else {
        RasterFormat::Png
    };
    let opts = RenderOptions {
        theme: if theme.eq_ignore_ascii_case("light") {
            Theme::Light
        } else {
            Theme::Dark
        },
        max_width_px: 1200,
        scale: 1.0,
        format: raster_format,
    };
    let asset = renderer.render(&content, &opts)?;

    let out_path = out.map(PathBuf::from).unwrap_or_else(|| {
        file.with_extension(if raster_format == RasterFormat::Svg {
            "svg"
        } else {
            "png"
        })
    });
    std::fs::write(&out_path, &asset.bytes)?;
    println!(
        "Wrote {} ({} bytes, {}x{})",
        out_path.display(),
        asset.bytes.len(),
        asset.width,
        asset.height
    );
    Ok(())
}
