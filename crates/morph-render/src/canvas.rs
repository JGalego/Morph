//! Shared SVG scene-building and rasterization infrastructure used by every
//! renderer in this crate.
//!
//! Renderers never touch SVG markup directly: they push rectangles, lines,
//! and text runs onto a [`Canvas`] at coordinates they compute themselves
//! (line height, column width, indent depth, ...), then call
//! [`Canvas::finish`] to get one complete, deterministic SVG document.
//! [`rasterize`] turns that document into the [`RenderedAsset`] the
//! `Renderer` trait promises, rasterizing to PNG only when asked.
//!
//! Fonts are embedded at compile time (see [`font_database`]) rather than
//! resolved from the host's font config, so a segment renders pixel-for-pixel
//! the same on a from-scratch container as it does on a developer's laptop.

use std::sync::{Arc, OnceLock};

use morph_core::prelude::*;

/// Family name recorded in the `name` table of the bundled DejaVu Sans Mono
/// faces. Must match exactly or `fontdb` falls back to a generic face.
pub const FONT_MONO: &str = "DejaVu Sans Mono";
/// Family name recorded in the `name` table of the bundled DejaVu Sans faces.
pub const FONT_SANS: &str = "DejaVu Sans";

/// Baseline-to-baseline spacing as a multiple of font size, used by every
/// renderer so vertical rhythm is consistent across content kinds.
pub(crate) const LINE_HEIGHT_RATIO: f32 = 1.5;
/// Outer page margin (in px) applied by every renderer around its content.
pub(crate) const MARGIN: f32 = 16.0;

/// Which embedded font family a text run should use. Kept separate from
/// bold/italic (those are per-run flags on [`Canvas::add_text`]) because a
/// single logical "mono" or "sans" choice maps to up to four physical faces
/// (regular/bold/italic/bold-italic) already loaded into [`font_database`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Mono,
    Sans,
}

impl Font {
    fn family(self) -> &'static str {
        match self {
            Font::Mono => FONT_MONO,
            Font::Sans => FONT_SANS,
        }
    }

    /// Estimated advance width of one character at `size`, used only for
    /// *layout* decisions (wrapping, canvas sizing) made before `usvg` does
    /// real text shaping. `Mono` is exact — DejaVu Sans Mono is a true
    /// fixed-width face — so canvases built from mono content are pixel
    /// accurate. `Sans` is an empirical average over mixed-case prose, which
    /// is good enough for greedy word-wrap without pulling in a shaping
    /// engine at layout time.
    pub fn char_advance(self, size: f32) -> f32 {
        match self {
            Font::Mono => size * 0.6,
            Font::Sans => size * 0.52,
        }
    }
}

/// A styled run of text produced by a highlighter (syntax, ANSI, or the
/// plain-text log heuristics), independent of where it ends up on the
/// canvas. Shared by `code.rs`, `markdown.rs`, and `log.rs` so all three
/// draw highlighted text through the exact same path.
#[derive(Debug, Clone)]
pub(crate) struct StyledSpan {
    pub text: String,
    pub color: String,
    pub bold: bool,
    pub italic: bool,
}

impl StyledSpan {
    pub(crate) fn plain(text: impl Into<String>, color: impl Into<String>) -> Self {
        StyledSpan {
            text: text.into(),
            color: color.into(),
            bold: false,
            italic: false,
        }
    }
}

/// A theme-selected set of colors every renderer draws from, so a JSON tree,
/// a code block, and a table all look like they belong to the same product.
///
/// Fields double up across content kinds by design: `accent_a` is "keyword"
/// in code, "object key" in JSON, "bullet marker" in markdown, and "ERROR" in
/// a log; `accent_e` is "punctuation" in JSON/code and "stack frame marker"
/// in a log. See each renderer for the specific mapping it uses.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: &'static str,
    pub foreground: &'static str,
    /// Comments, line-number gutters, faint indent guides.
    pub muted: &'static str,
    /// Table/code-panel borders and separators.
    pub border: &'static str,
    /// Table header row background.
    pub header_bg: &'static str,
    /// Table odd-row background stripe.
    pub row_alt_bg: &'static str,
    pub accent_a: &'static str,
    pub accent_b: &'static str,
    pub accent_c: &'static str,
    pub accent_d: &'static str,
    pub accent_e: &'static str,
    pub accent_f: &'static str,
}

impl Palette {
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Palette {
                background: "#1e1e2e",
                foreground: "#cdd6f4",
                muted: "#6c7086",
                border: "#45475a",
                header_bg: "#313244",
                row_alt_bg: "#262637",
                accent_a: "#f38ba8",
                accent_b: "#a6e3a1",
                accent_c: "#fab387",
                accent_d: "#89b4fa",
                accent_e: "#f5c2e7",
                accent_f: "#94e2d5",
            },
            Theme::Light => Palette {
                background: "#ffffff",
                foreground: "#1e1e2e",
                muted: "#8c8fa1",
                border: "#d0d7de",
                header_bg: "#f0f1f5",
                row_alt_bg: "#f6f7fa",
                accent_a: "#ae0060",
                accent_b: "#1a7f37",
                accent_c: "#bc5100",
                accent_d: "#0550ae",
                accent_e: "#8250df",
                accent_f: "#0f766e",
            },
        }
    }
}

/// Greedy word-wrap for proportional prose: fits as many whitespace-
/// separated words per line as fit within `max_chars`. A word longer than
/// `max_chars` on its own is hard-split, since it cannot wrap any other way.
///
/// This treats `text` as a single logical paragraph — callers that need to
/// preserve blank lines or block boundaries should call this once per
/// paragraph, not once for a whole document.
pub(crate) fn wrap_by_width(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > max_chars {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(max_chars) {
                lines.push(chunk.iter().collect::<String>());
            }
            continue;
        }
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word_len <= max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Splits a styled line into several lines of at most `max_chars` visible
/// characters each, potentially cutting a single span in two. Used by
/// `log.rs` so an extremely long terminal line still fits inside
/// `RenderOptions.max_width_px` instead of being clipped off-canvas.
pub(crate) fn wrap_styled_line(spans: &[StyledSpan], max_chars: usize) -> Vec<Vec<StyledSpan>> {
    let max_chars = max_chars.max(1);
    let mut lines: Vec<Vec<StyledSpan>> = vec![Vec::new()];
    let mut col = 0usize;

    for span in spans {
        let mut remaining: &str = &span.text;
        while !remaining.is_empty() {
            let space = max_chars - col;
            if space == 0 {
                lines.push(Vec::new());
                col = 0;
                continue;
            }
            let take = remaining.chars().count().min(space);
            let split_at = remaining
                .char_indices()
                .nth(take)
                .map(|(idx, _)| idx)
                .unwrap_or(remaining.len());
            let (head, tail) = remaining.split_at(split_at);
            if !head.is_empty() {
                // `lines` starts with one entry and is only ever pushed to,
                // never popped, so it is always non-empty here.
                if let Some(last) = lines.last_mut() {
                    last.push(StyledSpan {
                        text: head.to_string(),
                        color: span.color.clone(),
                        bold: span.bold,
                        italic: span.italic,
                    });
                }
            }
            col += take;
            remaining = tail;
        }
    }
    lines
}

/// One drawing operation queued on a [`Canvas`]. Kept as an internal enum
/// (rather than emitting SVG markup eagerly) so `Canvas` can compute its own
/// final width/height from the bounds of everything drawn before emitting
/// any markup.
#[derive(Debug, Clone)]
enum Shape {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: String,
        opacity: f32,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: String,
        width: f32,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        font: Font,
        size: f32,
        color: String,
        bold: bool,
        italic: bool,
    },
}

/// Accumulates shapes and text runs, then emits one SVG document sized to
/// fit exactly what was drawn (capped at the configured max width).
///
/// Callers are responsible for their own line wrapping and column layout —
/// `Canvas` only tracks the bounding box of what it's told to draw and turns
/// that into `width`/`height` attributes on the root `<svg>`.
pub struct Canvas {
    max_width_px: f32,
    shapes: Vec<Shape>,
    max_x: f32,
    max_y: f32,
}

impl Canvas {
    pub fn new(max_width_px: f32) -> Self {
        Canvas {
            max_width_px: max_width_px.max(1.0),
            shapes: Vec::new(),
            max_x: 0.0,
            max_y: 0.0,
        }
    }

    /// The configured cap (`RenderOptions.max_width_px`), independent of how
    /// much has been drawn so far. Renderers use this to size panels (e.g. a
    /// code block's background) to the remaining horizontal budget.
    pub fn budget_width(&self) -> f32 {
        self.max_width_px
    }

    fn extend(&mut self, x: f32, y: f32) {
        if x > self.max_x {
            self.max_x = x;
        }
        if y > self.max_y {
            self.max_y = y;
        }
    }

    pub fn add_rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: &str, opacity: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        self.extend(x + w, y + h);
        self.shapes.push(Shape::Rect {
            x,
            y,
            w,
            h,
            fill: fill.to_string(),
            opacity,
        });
    }

    pub fn add_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: &str, width: f32) {
        self.extend(x1.max(x2), y1.max(y2));
        self.shapes.push(Shape::Line {
            x1,
            y1,
            x2,
            y2,
            color: color.to_string(),
            width,
        });
    }

    /// `x`, `y` is the text baseline (matching SVG `<text>` semantics), not
    /// the top-left corner.
    #[allow(clippy::too_many_arguments)]
    pub fn add_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font: Font,
        size: f32,
        color: &str,
        bold: bool,
        italic: bool,
    ) {
        if text.is_empty() {
            return;
        }
        let w = text.chars().count() as f32 * font.char_advance(size);
        // Baseline `y` plus a rough descender allowance so the bounding box
        // comfortably contains the glyphs usvg will actually shape.
        self.extend(x + w, y + size * 0.3);
        self.shapes.push(Shape::Text {
            x,
            y,
            text: text.to_string(),
            font,
            size,
            color: color.to_string(),
            bold,
            italic,
        });
    }

    /// Running bounding-box width from everything drawn so far.
    pub fn content_width(&self) -> f32 {
        self.max_x
    }

    /// Running bounding-box height from everything drawn so far.
    pub fn content_height(&self) -> f32 {
        self.max_y
    }

    /// Emits the final SVG document: a background rect sized to the content
    /// (width capped at `budget_width()`, height from the tallest thing
    /// drawn) followed by every queued shape/text run in draw order.
    pub fn finish(&self, margin: f32, palette: &Palette) -> String {
        let width = (self.max_x + margin)
            .max(margin * 2.0)
            .min(self.max_width_px);
        let height = (self.max_y + margin).max(margin * 2.0);

        let mut svg = String::with_capacity(self.shapes.len() * 64 + 256);
        svg.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">",
            w = fmt_num(width),
            h = fmt_num(height),
        ));
        svg.push_str(&format!(
            "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"{bg}\"/>",
            w = fmt_num(width),
            h = fmt_num(height),
            bg = palette.background,
        ));

        for shape in &self.shapes {
            match shape {
                Shape::Rect {
                    x,
                    y,
                    w,
                    h,
                    fill,
                    opacity,
                } => {
                    svg.push_str(&format!(
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{fill}\" fill-opacity=\"{op}\"/>",
                        x = fmt_num(*x),
                        y = fmt_num(*y),
                        w = fmt_num(*w),
                        h = fmt_num(*h),
                        fill = fill,
                        op = fmt_num(*opacity),
                    ));
                }
                Shape::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    color,
                    width,
                } => {
                    svg.push_str(&format!(
                        "<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" stroke=\"{color}\" stroke-width=\"{sw}\"/>",
                        x1 = fmt_num(*x1),
                        y1 = fmt_num(*y1),
                        x2 = fmt_num(*x2),
                        y2 = fmt_num(*y2),
                        color = color,
                        sw = fmt_num(*width),
                    ));
                }
                Shape::Text {
                    x,
                    y,
                    text,
                    font,
                    size,
                    color,
                    bold,
                    italic,
                } => {
                    let weight = if *bold { " font-weight=\"bold\"" } else { "" };
                    let style = if *italic {
                        " font-style=\"italic\""
                    } else {
                        ""
                    };
                    svg.push_str(&format!(
                        "<text x=\"{x}\" y=\"{y}\" font-family=\"{ff}\" font-size=\"{size}\" fill=\"{color}\"{weight}{style} xml:space=\"preserve\">{text}</text>",
                        x = fmt_num(*x),
                        y = fmt_num(*y),
                        ff = font.family(),
                        size = fmt_num(*size),
                        color = color,
                        weight = weight,
                        style = style,
                        text = escape_xml(text),
                    ));
                }
            }
        }

        svg.push_str("</svg>");
        svg
    }
}

fn fmt_num(n: f32) -> String {
    format!("{:.2}", n)
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' => out.push_str("    "),
            _ => out.push(c),
        }
    }
    out
}

/// Lazily-initialized, process-wide font database seeded with Morph's
/// bundled DejaVu faces. `OnceLock` (not a per-call load) matters here: each
/// bundled face is a few hundred KB and parsing them is not free, and every
/// renderer call goes through this on the hot path.
fn font_database() -> &'static Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        for bytes in [
            &include_bytes!("../../../assets/fonts/DejaVuSans.ttf")[..],
            &include_bytes!("../../../assets/fonts/DejaVuSans-Bold.ttf")[..],
            &include_bytes!("../../../assets/fonts/DejaVuSans-Oblique.ttf")[..],
            &include_bytes!("../../../assets/fonts/DejaVuSansMono.ttf")[..],
            &include_bytes!("../../../assets/fonts/DejaVuSansMono-Bold.ttf")[..],
        ] {
            db.load_font_data(bytes.to_vec());
        }
        Arc::new(db)
    })
}

/// Parses `svg` and, depending on `opts.format`, either hands back the raw
/// SVG bytes or rasterizes to PNG. This is the only place in the crate that
/// touches `usvg`/`resvg`/`tiny-skia` directly.
pub fn rasterize(svg: &str, opts: &RenderOptions) -> Result<RenderedAsset> {
    let mut usvg_opts = usvg::Options {
        fontdb: font_database().clone(),
        ..usvg::Options::default()
    };
    // Bundled faces cover every glyph our renderers ever emit; a default
    // family avoids usvg silently falling back to a system font search.
    usvg_opts.font_family = FONT_SANS.to_string();

    let tree = usvg::Tree::from_str(svg, &usvg_opts)
        .map_err(|e| GatewayError::Render(format!("failed to parse generated SVG: {e}")))?;
    let size = tree.size();
    let width = size.width().round().max(1.0) as u32;
    let height = size.height().round().max(1.0) as u32;

    match opts.format {
        RasterFormat::Svg => Ok(RenderedAsset {
            mime: "image/svg+xml".to_string(),
            bytes: svg.as_bytes().to_vec(),
            width,
            height,
            alt_text: None,
        }),
        RasterFormat::Png => {
            let scale = if opts.scale.is_finite() && opts.scale > 0.0 {
                opts.scale
            } else {
                1.0
            };
            let px_w = ((width as f32) * scale).round().max(1.0) as u32;
            let px_h = ((height as f32) * scale).round().max(1.0) as u32;
            let mut pixmap = tiny_skia::Pixmap::new(px_w, px_h)
                .ok_or_else(|| GatewayError::Render("computed a zero-sized canvas".to_string()))?;
            let transform = tiny_skia::Transform::from_scale(scale, scale);
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            let bytes = pixmap
                .encode_png()
                .map_err(|e| GatewayError::Render(format!("PNG encoding failed: {e}")))?;
            Ok(RenderedAsset {
                mime: "image/png".to_string(),
                bytes,
                width: px_w,
                height: px_h,
                alt_text: None,
            })
        }
        RasterFormat::Jpeg | RasterFormat::WebP => Err(GatewayError::Unsupported(format!(
            "morph-render v1 only produces svg/png output, got {:?}",
            opts.format
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_by_width_breaks_on_word_boundaries() {
        let lines = wrap_by_width("the quick brown fox jumps", 10);
        assert_eq!(lines, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn wrap_by_width_hard_splits_overlong_word() {
        let lines = wrap_by_width("supercalifragilistic", 6);
        assert_eq!(lines, vec!["superc", "alifra", "gilist", "ic"]);
    }

    #[test]
    fn wrap_by_width_empty_input_yields_one_empty_line() {
        assert_eq!(wrap_by_width("", 10), vec![""]);
    }

    #[test]
    fn wrap_styled_line_splits_a_span_across_lines() {
        let spans = vec![StyledSpan::plain("abcdefgh", "#fff")];
        let wrapped = wrap_styled_line(&spans, 3);
        let texts: Vec<&str> = wrapped.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["abc", "def", "gh"]);
    }

    #[test]
    fn canvas_caps_width_at_budget() {
        let mut canvas = Canvas::new(100.0);
        canvas.add_text(
            0.0,
            0.0,
            &"x".repeat(500),
            Font::Mono,
            14.0,
            "#fff",
            false,
            false,
        );
        let palette = Palette::for_theme(Theme::Dark);
        let svg = canvas.finish(MARGIN, &palette);
        assert!(svg.contains("width=\"100.00\""));
    }

    #[test]
    fn rasterize_png_has_magic_bytes() {
        let mut canvas = Canvas::new(400.0);
        let palette = Palette::for_theme(Theme::Dark);
        canvas.add_text(
            10.0,
            20.0,
            "hello",
            Font::Mono,
            14.0,
            palette.foreground,
            false,
            false,
        );
        let svg = canvas.finish(MARGIN, &palette);
        let opts = RenderOptions {
            format: RasterFormat::Png,
            ..RenderOptions::default()
        };
        let asset = rasterize(&svg, &opts).expect("rasterize should succeed");
        assert!(asset.bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(asset.width > 0 && asset.height > 0);
    }

    #[test]
    fn rasterize_svg_passthrough() {
        let mut canvas = Canvas::new(400.0);
        let palette = Palette::for_theme(Theme::Light);
        canvas.add_text(
            10.0,
            20.0,
            "hello",
            Font::Sans,
            14.0,
            palette.foreground,
            false,
            false,
        );
        let svg = canvas.finish(MARGIN, &palette);
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            ..RenderOptions::default()
        };
        let asset = rasterize(&svg, &opts).expect("rasterize should succeed");
        assert_eq!(asset.mime, "image/svg+xml");
        assert!(asset.bytes.starts_with(b"<svg"));
    }

    #[test]
    fn rasterize_rejects_jpeg() {
        let canvas = Canvas::new(100.0);
        let palette = Palette::for_theme(Theme::Dark);
        let svg = canvas.finish(MARGIN, &palette);
        let opts = RenderOptions {
            format: RasterFormat::Jpeg,
            ..RenderOptions::default()
        };
        assert!(rasterize(&svg, &opts).is_err());
    }
}
