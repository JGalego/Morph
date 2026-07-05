//! Syntax-highlighted code rendering, backed by `syntect`.
//!
//! [`render_code_block`] is the shared drawing routine: `CodeRenderer` calls
//! it for a whole segment, and `markdown.rs` calls the exact same function
//! for each fenced code block it finds, so a Rust snippet looks identical
//! whether it arrived as its own `ContentKind::Code` segment or embedded in
//! a markdown document.

use std::sync::OnceLock;

use morph_core::prelude::*;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme as SyntectTheme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::canvas::{Canvas, Font, Palette, StyledSpan, LINE_HEIGHT_RATIO, MARGIN};

/// Default font size for code segments (px), reused by `markdown.rs` for
/// fenced code blocks so a snippet looks the same whether it's its own
/// `ContentKind::Code` segment or embedded in a document.
pub(crate) const CODE_FONT_SIZE: f32 = 14.0;

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

fn resolve_syntax<'a>(ss: &'a SyntaxSet, language: Option<&str>) -> &'a SyntaxReference {
    language
        .and_then(|lang| ss.find_syntax_by_token(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Picks the bundled syntect theme matching Morph's dark/light split, with a
/// graceful fallback to whatever theme happens to be loaded first rather
/// than panicking if a theme name ever goes missing from a future syntect
/// release.
fn resolve_theme(ts: &ThemeSet, theme: Theme) -> Result<&SyntectTheme> {
    let name = match theme {
        Theme::Dark => "base16-ocean.dark",
        Theme::Light => "InspiredGitHub",
    };
    ts.themes
        .get(name)
        .or_else(|| ts.themes.values().next())
        .ok_or_else(|| GatewayError::Render("no syntect themes are available".to_string()))
}

fn color_to_hex(c: syntect::highlighting::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Runs `source` through syntect's incremental highlighter one line at a
/// time, translating each `(Style, &str)` token into a [`StyledSpan`].
/// Falls back to an unstyled span for any line syntect fails to parse
/// (malformed input should degrade to plain text, not abort rendering).
pub(crate) fn highlight_lines(
    ss: &SyntaxSet,
    syntax: &SyntaxReference,
    syn_theme: &SyntectTheme,
    source: &str,
    default_color: &str,
) -> Vec<Vec<StyledSpan>> {
    let mut highlighter = HighlightLines::new(syntax, syn_theme);
    let mut out = Vec::new();

    for line in LinesWithEndings::from(source) {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let ranges = highlighter.highlight_line(line, ss).unwrap_or_default();
        if ranges.is_empty() {
            out.push(vec![StyledSpan::plain(trimmed, default_color)]);
            continue;
        }
        let mut spans = Vec::with_capacity(ranges.len());
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            spans.push(StyledSpan {
                text: text.to_string(),
                color: color_to_hex(style.foreground),
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            });
        }
        out.push(spans);
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

/// Gutter width wide enough for right-aligned line numbers up to `n_lines`,
/// plus a little breathing room either side.
fn gutter_width(n_lines: usize, font_size: f32) -> f32 {
    let digits = n_lines.max(1).to_string().len().max(2);
    (digits as f32 + 2.0) * Font::Mono.char_advance(font_size)
}

/// Draws already-highlighted lines starting at `(x, y)`, optionally with a
/// right-aligned, muted-color line-number gutter. Returns the y coordinate
/// immediately below the last line drawn.
fn draw_highlighted_lines(
    canvas: &mut Canvas,
    lines: &[Vec<StyledSpan>],
    x: f32,
    y: f32,
    font_size: f32,
    palette: &Palette,
    show_line_numbers: bool,
) -> f32 {
    let line_h = font_size * LINE_HEIGHT_RATIO;
    let gutter_w = if show_line_numbers {
        gutter_width(lines.len(), font_size)
    } else {
        0.0
    };
    let char_w = Font::Mono.char_advance(font_size);
    let mut cursor_y = y;

    for (i, spans) in lines.iter().enumerate() {
        let baseline = cursor_y + font_size;
        if show_line_numbers {
            let n = (i + 1).to_string();
            let num_w = n.chars().count() as f32 * char_w;
            let num_x = x + gutter_w - num_w - char_w;
            canvas.add_text(
                num_x,
                baseline,
                &n,
                Font::Mono,
                font_size,
                palette.muted,
                false,
                false,
            );
        }
        let mut cx = x + gutter_w;
        for span in spans {
            canvas.add_text(
                cx,
                baseline,
                &span.text,
                Font::Mono,
                font_size,
                &span.color,
                span.bold,
                span.italic,
            );
            cx += span.text.chars().count() as f32 * char_w;
        }
        cursor_y += line_h;
    }
    cursor_y
}

/// Renders `source` as a highlighted code block: a background panel, an
/// optional line-number gutter, and one `<text>` run per syntax token.
/// Shared by [`CodeRenderer`] and `markdown.rs`'s fenced-code-block handling.
///
/// Returns the y coordinate immediately below the block, so callers can
/// keep laying out content underneath it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_code_block(
    canvas: &mut Canvas,
    source: &str,
    language: Option<&str>,
    theme: Theme,
    palette: &Palette,
    x: f32,
    y: f32,
    font_size: f32,
    show_line_numbers: bool,
) -> Result<f32> {
    let ss = syntax_set();
    let ts = theme_set();
    let syntax = resolve_syntax(ss, language);
    let syn_theme = resolve_theme(ts, theme)?;
    let lines = highlight_lines(ss, syntax, syn_theme, source, palette.foreground);

    let line_h = font_size * LINE_HEIGHT_RATIO;
    let pad = font_size * 0.5;
    let gutter_w = if show_line_numbers {
        gutter_width(lines.len(), font_size)
    } else {
        0.0
    };
    let char_w = Font::Mono.char_advance(font_size);
    let longest_chars = lines
        .iter()
        .map(|spans| spans.iter().map(|s| s.text.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let natural_w = gutter_w + longest_chars as f32 * char_w + pad * 2.0;
    let available_w = (canvas.budget_width() - x - MARGIN * 0.5).max(natural_w.min(160.0));
    let block_w = natural_w.min(available_w);
    let panel_h = pad * 2.0 + (lines.len().max(1) as f32) * line_h;

    canvas.add_rect(x, y, block_w, panel_h, palette.header_bg, 1.0);
    if show_line_numbers && gutter_w > 0.0 {
        canvas.add_rect(x + pad, y, gutter_w, panel_h, palette.border, 0.25);
        canvas.add_line(
            x + pad + gutter_w,
            y,
            x + pad + gutter_w,
            y + panel_h,
            palette.border,
            1.0,
        );
    }

    draw_highlighted_lines(
        canvas,
        &lines,
        x + pad,
        y + pad,
        font_size,
        palette,
        show_line_numbers,
    );

    Ok(y + panel_h)
}

/// Renders `ContentKind::Code` segments: syntax-highlighted text with a
/// line-number gutter, using the detected language when known and falling
/// back to plain-text tokenization otherwise.
pub struct CodeRenderer;

impl CodeRenderer {
    pub fn new() -> Self {
        CodeRenderer
    }
}

impl Default for CodeRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl morph_core::traits::Renderer for CodeRenderer {
    fn name(&self) -> &str {
        "code"
    }

    fn supports(&self, kind: ContentKind) -> bool {
        kind == ContentKind::Code
    }

    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset> {
        let palette = Palette::for_theme(opts.theme);
        let mut canvas = Canvas::new(opts.max_width_px as f32);
        render_code_block(
            &mut canvas,
            &content.raw,
            content.language.as_deref(),
            opts.theme,
            &palette,
            MARGIN,
            MARGIN,
            CODE_FONT_SIZE,
            true,
        )?;
        let svg = canvas.finish(MARGIN, &palette);
        let mut asset = crate::canvas::rasterize(&svg, opts)?;
        asset.alt_text = Some(format!(
            "Syntax-highlighted {} code, {} line(s)",
            content.language.as_deref().unwrap_or("plain-text"),
            content.metrics.line_count.max(1),
        ));
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    fn sample() -> DetectedContent {
        DetectedContent {
            kind: ContentKind::Code,
            raw: "fn main() {\n    println!(\"hi\");\n}\n".to_string(),
            confidence: 1.0,
            metrics: ContentMetrics::from_text("fn main() {\n    println!(\"hi\");\n}\n"),
            language: Some("rust".to_string()),
            message_index: None,
        }
    }

    #[test]
    fn snapshot_rust_code_svg() {
        let renderer = CodeRenderer::new();
        let content = sample();
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&content, &opts)
            .expect("render should succeed");
        let svg = String::from_utf8(asset.bytes).expect("svg should be utf8");
        assert_snapshot!(svg);
    }

    #[test]
    fn rasterize_png_has_magic_bytes_and_size() {
        use morph_core::traits::Renderer;
        let renderer = CodeRenderer::new();
        let content = sample();
        let opts = RenderOptions {
            format: RasterFormat::Png,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&content, &opts)
            .expect("render should succeed");
        assert!(asset.bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(asset.width > 0 && asset.height > 0);
    }

    #[test]
    fn falls_back_to_plain_text_for_unknown_language() {
        let renderer = CodeRenderer::new();
        let content = DetectedContent {
            language: Some("not-a-real-language".to_string()),
            ..sample()
        };
        let opts = RenderOptions::default();
        let asset = renderer
            .render(&content, &opts)
            .expect("render should still succeed");
        assert!(asset.width > 0);
    }

    #[test]
    fn supports_only_code_kind() {
        let renderer = CodeRenderer::new();
        assert!(morph_core::traits::Renderer::supports(
            &renderer,
            ContentKind::Code
        ));
        assert!(!morph_core::traits::Renderer::supports(
            &renderer,
            ContentKind::Json
        ));
    }
}
