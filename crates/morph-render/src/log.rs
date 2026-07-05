//! Terminal log / stack trace rendering.
//!
//! Two highlighting paths, chosen per physical line:
//! - Lines carrying ANSI SGR escape codes (`\x1b[...m`) are parsed into
//!   styled spans that track the escape codes' foreground color and
//!   bold/italic state.
//! - Lines with no escape codes fall back to a small set of heuristics that
//!   color ISO-8601-ish timestamps and common `ERROR`/`WARN`/`at `/`File "`
//!   markers, so plain (non-TTY-captured) logs still get useful highlights.

use std::sync::OnceLock;

use morph_core::prelude::*;
use regex::Regex;

use crate::canvas::{
    wrap_styled_line, Canvas, Font, Palette, StyledSpan, LINE_HEIGHT_RATIO, MARGIN,
};

const LOG_FONT_SIZE: f32 = 13.0;

fn csi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;:?]*[A-Za-z]").expect("static regex is valid"))
}

fn marker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?P<timestamp>\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?)",
            r"|(?P<error>\b(?:ERROR|FATAL|PANIC|Traceback)\b)",
            r"|(?P<warn>\b(?:WARN|WARNING)\b)",
            r"|(?P<info>\b(?:INFO|DEBUG|TRACE)\b)",
            r#"|(?P<stackat>\bat\s+\S+)"#,
            r#"|(?P<stackfile>File\s+"[^"]+")"#,
        ))
        .expect("static regex is valid")
    })
}

#[derive(Debug, Default, Clone)]
struct AnsiState {
    /// Base SGR color code (30-37 / 90-97), remapped through the theme
    /// palette so ANSI colors stay legible against Morph's own background.
    fg_base: Option<u16>,
    /// Explicit 256-color / truecolor foreground, rendered verbatim since
    /// the source asked for that exact color.
    fg_hex: Option<String>,
    bold: bool,
    italic: bool,
}

impl AnsiState {
    fn reset(&mut self) {
        *self = AnsiState::default();
    }

    fn color<'p>(&self, palette: &'p Palette) -> std::borrow::Cow<'p, str> {
        if let Some(hex) = &self.fg_hex {
            return std::borrow::Cow::Owned(hex.clone());
        }
        if let Some(code) = self.fg_base {
            return std::borrow::Cow::Borrowed(ansi_base_color(code, palette));
        }
        std::borrow::Cow::Borrowed(palette.foreground)
    }
}

/// Maps the 16 standard/bright ANSI SGR color codes onto Morph's theme
/// palette, so a red `ERROR` from a captured terminal session uses the same
/// red as everything else Morph colors red — rather than clashing terminal
/// defaults that were tuned for a black terminal background.
fn ansi_base_color(code: u16, palette: &Palette) -> &'static str {
    match code {
        30 | 90 => palette.muted,
        31 | 91 => palette.accent_a,
        32 | 92 => palette.accent_b,
        33 | 93 => palette.accent_c,
        34 | 94 => palette.accent_d,
        35 | 95 => palette.accent_e,
        36 | 96 => palette.accent_f,
        _ => palette.foreground,
    }
}

/// Standard xterm 256-color palette formula for indices 16-255 (6x6x6 color
/// cube plus a 24-step grayscale ramp). Indices 0-15 fall back to the same
/// theme remapping as basic SGR codes, for consistency.
fn ansi_256_to_hex(n: u8, palette: &Palette) -> String {
    match n {
        0..=7 => ansi_base_color(30 + n as u16, palette).to_string(),
        8..=15 => ansi_base_color(82 + n as u16, palette).to_string(),
        16..=231 => {
            let m = n - 16;
            let r = m / 36;
            let g = (m % 36) / 6;
            let b = m % 6;
            let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            format!("#{:02x}{:02x}{:02x}", scale(r), scale(g), scale(b))
        }
        232..=255 => {
            let level = 8 + (n - 232) * 10;
            format!("#{:02x}{:02x}{:02x}", level, level, level)
        }
    }
}

fn apply_sgr(seq: &str, state: &mut AnsiState, palette: &Palette) {
    // `seq` is `\x1b[<params>m` (checked by the caller); strip `ESC[` and `m`.
    let body = &seq[2..seq.len().saturating_sub(1)];
    if body.is_empty() {
        state.reset();
        return;
    }
    let mut codes = body.split(';').map(|p| p.parse::<u16>().unwrap_or(0));
    while let Some(code) = codes.next() {
        match code {
            0 => state.reset(),
            1 => state.bold = true,
            3 => state.italic = true,
            22 => state.bold = false,
            23 => state.italic = false,
            30..=37 | 90..=97 => {
                state.fg_base = Some(code);
                state.fg_hex = None;
            }
            39 => {
                state.fg_base = None;
                state.fg_hex = None;
            }
            38 => match codes.next() {
                Some(5) => {
                    if let Some(n) = codes.next() {
                        state.fg_hex = Some(ansi_256_to_hex(n.min(255) as u8, palette));
                        state.fg_base = None;
                    }
                }
                Some(2) => {
                    let r = codes.next().unwrap_or(0).min(255) as u8;
                    let g = codes.next().unwrap_or(0).min(255) as u8;
                    let b = codes.next().unwrap_or(0).min(255) as u8;
                    state.fg_hex = Some(format!("#{r:02x}{g:02x}{b:02x}"));
                    state.fg_base = None;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Parses one line containing ANSI escape codes into styled spans. Non-SGR
/// CSI sequences (cursor movement, screen clears, ...) are recognized and
/// silently dropped rather than leaking into the rendered text, since this
/// renderer produces a static image with no terminal to move a cursor on.
fn highlight_ansi_line(line: &str, palette: &Palette) -> Vec<StyledSpan> {
    let mut state = AnsiState::default();
    let mut spans = Vec::new();
    let mut last = 0;

    for m in csi_regex().find_iter(line) {
        if m.start() > last {
            push_plain(&mut spans, &line[last..m.start()], &state, palette);
        }
        let seq = &line[m.start()..m.end()];
        if seq.ends_with('m') {
            apply_sgr(seq, &mut state, palette);
        }
        last = m.end();
    }
    if last < line.len() {
        push_plain(&mut spans, &line[last..], &state, palette);
    }
    if spans.is_empty() {
        spans.push(StyledSpan::plain(line, palette.foreground));
    }
    spans
}

fn push_plain(spans: &mut Vec<StyledSpan>, text: &str, state: &AnsiState, palette: &Palette) {
    if text.is_empty() {
        return;
    }
    spans.push(StyledSpan {
        text: text.to_string(),
        color: state.color(palette).into_owned(),
        bold: state.bold,
        italic: state.italic,
    });
}

/// Highlights a plain (no ANSI) line via heuristic markers: timestamps,
/// severity keywords, and common stack-frame prefixes.
fn highlight_plain_line(line: &str, palette: &Palette) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut last = 0;

    for caps in marker_regex().captures_iter(line) {
        let Some(m) = caps.get(0) else { continue };
        if m.start() > last {
            spans.push(StyledSpan::plain(
                &line[last..m.start()],
                palette.foreground,
            ));
        }
        let (color, bold) = if caps.name("timestamp").is_some() {
            (palette.accent_c, false)
        } else if caps.name("error").is_some() {
            (palette.accent_a, true)
        } else if caps.name("warn").is_some() {
            (palette.accent_c, true)
        } else if caps.name("info").is_some() {
            (palette.accent_d, false)
        } else {
            (palette.accent_e, false)
        };
        spans.push(StyledSpan {
            text: m.as_str().to_string(),
            color: color.to_string(),
            bold,
            italic: false,
        });
        last = m.end();
    }
    if last < line.len() {
        spans.push(StyledSpan::plain(&line[last..], palette.foreground));
    }
    if spans.is_empty() {
        spans.push(StyledSpan::plain(line, palette.foreground));
    }
    spans
}

/// Renders `ContentKind::TerminalLog` and `ContentKind::StackTrace`
/// segments: ANSI-aware where the source carries escape codes, heuristic
/// keyword/timestamp highlighting otherwise. Long lines are hard-wrapped
/// (this content is monospace, so a character budget is exact) rather than
/// silently clipped, since the most important part of a log line is often
/// near the end (a file path, an error message).
pub struct LogRenderer;

impl LogRenderer {
    pub fn new() -> Self {
        LogRenderer
    }
}

impl Default for LogRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl morph_core::traits::Renderer for LogRenderer {
    fn name(&self) -> &str {
        "log"
    }

    fn supports(&self, kind: ContentKind) -> bool {
        matches!(kind, ContentKind::TerminalLog | ContentKind::StackTrace)
    }

    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset> {
        let palette = Palette::for_theme(opts.theme);
        let mut canvas = Canvas::new(opts.max_width_px as f32);
        let line_h = LOG_FONT_SIZE * LINE_HEIGHT_RATIO;
        let char_w = Font::Mono.char_advance(LOG_FONT_SIZE);
        let max_chars = (((opts.max_width_px as f32) - 2.0 * MARGIN) / char_w)
            .floor()
            .max(10.0) as usize;

        let mut y = MARGIN;
        let mut line_count = 0usize;
        for raw_line in content.raw.lines() {
            let spans = if raw_line.contains('\u{1b}') {
                highlight_ansi_line(raw_line, &palette)
            } else {
                highlight_plain_line(raw_line, &palette)
            };
            for wrapped in wrap_styled_line(&spans, max_chars) {
                let baseline = y + LOG_FONT_SIZE;
                let mut x = MARGIN;
                for span in &wrapped {
                    canvas.add_text(
                        x,
                        baseline,
                        &span.text,
                        Font::Mono,
                        LOG_FONT_SIZE,
                        &span.color,
                        span.bold,
                        span.italic,
                    );
                    x += span.text.chars().count() as f32 * char_w;
                }
                y += line_h;
                line_count += 1;
            }
        }
        let svg = canvas.finish(MARGIN, &palette);
        let mut asset = crate::canvas::rasterize(&svg, opts)?;
        asset.alt_text = Some(format!("Terminal log, {line_count} rendered line(s)"));
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use morph_core::traits::Renderer;

    fn ansi_sample() -> DetectedContent {
        let raw = "\u{1b}[31mERROR\u{1b}[0m connection refused\n\u{1b}[32mINFO\u{1b}[0m listening on :8080\n".to_string();
        DetectedContent {
            kind: ContentKind::TerminalLog,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }

    fn plain_stacktrace_sample() -> DetectedContent {
        let raw = "2024-01-02T03:04:05Z ERROR panicked at src/main.rs:42\n  at main (src/main.rs:42)\n  File \"app.py\", line 10, in <module>\n".to_string();
        DetectedContent {
            kind: ContentKind::StackTrace,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }

    #[test]
    fn ansi_line_parses_fg_color_and_reset() {
        let palette = Palette::for_theme(Theme::Dark);
        let spans = highlight_ansi_line("\u{1b}[31mERROR\u{1b}[0m ok", &palette);
        assert_eq!(spans[0].text, "ERROR");
        assert_eq!(spans[0].color, palette.accent_a);
        assert_eq!(spans[1].text, " ok");
        assert_eq!(spans[1].color, palette.foreground);
    }

    #[test]
    fn plain_line_highlights_timestamp_and_level() {
        let palette = Palette::for_theme(Theme::Dark);
        let spans = highlight_plain_line("2024-01-02T03:04:05Z ERROR boom", &palette);
        assert_eq!(spans[0].text, "2024-01-02T03:04:05Z");
        assert_eq!(spans[0].color, palette.accent_c);
        assert!(spans
            .iter()
            .any(|s| s.text == "ERROR" && s.color == palette.accent_a));
    }

    #[test]
    fn snapshot_ansi_log_svg() {
        let renderer = LogRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&ansi_sample(), &opts)
            .expect("render should succeed");
        let svg = String::from_utf8(asset.bytes).expect("svg should be utf8");
        assert_snapshot!(svg);
    }

    #[test]
    fn snapshot_plain_stacktrace_svg() {
        let renderer = LogRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            theme: Theme::Light,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&plain_stacktrace_sample(), &opts)
            .expect("render should succeed");
        let svg = String::from_utf8(asset.bytes).expect("svg should be utf8");
        assert_snapshot!(svg);
    }

    #[test]
    fn rasterize_png_has_magic_bytes_and_size() {
        let renderer = LogRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Png,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&ansi_sample(), &opts)
            .expect("render should succeed");
        assert!(asset.bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(asset.width > 0 && asset.height > 0);
    }

    #[test]
    fn long_line_is_hard_wrapped_not_clipped() {
        let renderer = LogRenderer::new();
        let raw = "x".repeat(500) + "\n";
        let content = DetectedContent {
            kind: ContentKind::TerminalLog,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        };
        let opts = RenderOptions {
            max_width_px: 400,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&content, &opts)
            .expect("render should succeed");
        // Height should reflect several wrapped rows, not a single clipped one.
        assert!(asset.height as f32 > LOG_FONT_SIZE * LINE_HEIGHT_RATIO * 3.0);
    }

    #[test]
    fn supports_terminal_log_and_stack_trace() {
        let renderer = LogRenderer::new();
        assert!(renderer.supports(ContentKind::TerminalLog));
        assert!(renderer.supports(ContentKind::StackTrace));
        assert!(!renderer.supports(ContentKind::Code));
    }
}
