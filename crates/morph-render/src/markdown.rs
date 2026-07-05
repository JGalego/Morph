//! Markdown document rendering: headers, lists, pipe-tables, fenced code
//! blocks, and paragraphs, laid out with a simple heading-size hierarchy and
//! consistent block spacing. This is deliberately a block-level-only
//! renderer — inline emphasis (`**bold**`, `` `code` ``, links) is left as
//! literal text rather than specially styled; see the crate-level notes for
//! why that's a deliberate v1 scope cut, not an oversight.

use std::sync::OnceLock;

use morph_core::prelude::*;
use regex::Regex;

use crate::canvas::{wrap_by_width, Canvas, Font, Palette, LINE_HEIGHT_RATIO, MARGIN};
use crate::code;
use crate::table::{self, Table};

const BODY_SIZE: f32 = 15.0;
const HEADING_SIZES: [f32; 6] = [28.0, 24.0, 20.0, 18.0, 16.0, 15.0];
const BULLET_INDENT_PX: f32 = 20.0;

#[derive(Debug, Clone)]
enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Bullet {
        depth: usize,
        text: String,
    },
    Ordered {
        depth: usize,
        marker: String,
        text: String,
    },
    Code {
        language: Option<String>,
        source: String,
    },
    Table(Table),
    Paragraph(String),
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s{0,3}(#{1,6})\s+(.*)$").expect("static regex is valid"))
}

fn bullet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\s*)[-*+]\s+(.*)$").expect("static regex is valid"))
}

fn ordered_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\s*)(\d+)[.)]\s+(.*)$").expect("static regex is valid"))
}

fn fence_open(trimmed: &str) -> Option<(&'static str, Option<String>)> {
    for fence in ["```", "~~~"] {
        if let Some(rest) = trimmed.strip_prefix(fence) {
            let lang = rest.trim();
            return Some((
                fence,
                if lang.is_empty() {
                    None
                } else {
                    Some(lang.to_string())
                },
            ));
        }
    }
    None
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let caps = heading_re().captures(line)?;
    let level = caps.get(1)?.as_str().len().min(6) as u8;
    Some((level, caps.get(2)?.as_str().to_string()))
}

fn parse_bullet(line: &str) -> Option<(usize, String)> {
    let caps = bullet_re().captures(line)?;
    Some((
        caps.get(1)?.as_str().len(),
        caps.get(2)?.as_str().to_string(),
    ))
}

fn parse_ordered(line: &str) -> Option<(usize, String, String)> {
    let caps = ordered_re().captures(line)?;
    Some((
        caps.get(1)?.as_str().len(),
        caps.get(2)?.as_str().to_string(),
        caps.get(3)?.as_str().to_string(),
    ))
}

/// Splits `text` into a flat sequence of block-level elements. Deliberately
/// simple line-based scanning rather than a general CommonMark parser: this
/// covers exactly the block types the renderer knows how to draw, and
/// anything else degrades gracefully to a paragraph.
fn parse_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed_start = line.trim_start();

        if trimmed_start.trim().is_empty() {
            i += 1;
            continue;
        }

        if let Some((fence, language)) = fence_open(trimmed_start) {
            i += 1;
            let mut code_lines = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume the closing fence
            }
            let mut source = code_lines.join("\n");
            if !code_lines.is_empty() {
                source.push('\n');
            }
            blocks.push(Block::Code { language, source });
            continue;
        }

        if let Some((level, text)) = parse_heading(line) {
            blocks.push(Block::Heading { level, text });
            i += 1;
            continue;
        }

        if line.contains('|') {
            let mut j = i;
            while j < lines.len() && !lines[j].trim().is_empty() && lines[j].contains('|') {
                j += 1;
            }
            if let Some(parsed) = table::parse_pipe_table(&lines[i..j].join("\n")) {
                blocks.push(Block::Table(parsed));
                i = j;
                continue;
            }
            // Not actually a table (e.g. a stray "|" in prose) — fall
            // through and let the paragraph/list handling below take it.
        }

        if let Some((indent, text)) = parse_bullet(line) {
            blocks.push(Block::Bullet {
                depth: indent / 2,
                text,
            });
            i += 1;
            continue;
        }

        if let Some((indent, num, text)) = parse_ordered(line) {
            blocks.push(Block::Ordered {
                depth: indent / 2,
                marker: format!("{num}."),
                text,
            });
            i += 1;
            continue;
        }

        let mut para = vec![line.trim()];
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            if l.trim().is_empty()
                || fence_open(l.trim_start()).is_some()
                || parse_heading(l).is_some()
                || parse_bullet(l).is_some()
                || parse_ordered(l).is_some()
                || l.contains('|')
            {
                break;
            }
            para.push(l.trim());
            i += 1;
        }
        blocks.push(Block::Paragraph(para.join(" ")));
    }

    blocks
}

fn max_chars_for(canvas: &Canvas, x: f32, font: Font, size: f32) -> usize {
    (((canvas.budget_width() - x - MARGIN) / font.char_advance(size)).floor() as isize).max(4)
        as usize
}

/// Renders `ContentKind::Markdown` segments: this doubles as the project's
/// "Document" renderer, since a markdown document is exactly headers, lists,
/// tables, code, and paragraphs.
pub struct MarkdownRenderer;

impl MarkdownRenderer {
    pub fn new() -> Self {
        MarkdownRenderer
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl morph_core::traits::Renderer for MarkdownRenderer {
    fn name(&self) -> &str {
        "markdown"
    }

    fn supports(&self, kind: ContentKind) -> bool {
        // Plain prose is just a document with no block-level structure —
        // `parse_blocks` already degrades any unrecognized line to a
        // paragraph, so no separate code path is needed for it.
        matches!(kind, ContentKind::Markdown | ContentKind::PlainText)
    }

    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset> {
        let palette = Palette::for_theme(opts.theme);
        let mut canvas = Canvas::new(opts.max_width_px as f32);
        let blocks = parse_blocks(&content.raw);
        let body_line_h = BODY_SIZE * LINE_HEIGHT_RATIO;

        let mut y = MARGIN;
        let mut first = true;
        for block in &blocks {
            if !first {
                y += if matches!(block, Block::Heading { .. }) {
                    20.0
                } else {
                    10.0
                };
            }
            first = false;

            match block {
                Block::Heading { level, text } => {
                    let size = HEADING_SIZES[(*level - 1) as usize];
                    let line_h = size * LINE_HEIGHT_RATIO;
                    let max_chars = max_chars_for(&canvas, MARGIN, Font::Sans, size);
                    let wrapped = wrap_by_width(text, max_chars);
                    for (wi, wline) in wrapped.iter().enumerate() {
                        let baseline = y + wi as f32 * line_h + size;
                        canvas.add_text(
                            MARGIN,
                            baseline,
                            wline,
                            Font::Sans,
                            size,
                            palette.foreground,
                            true,
                            false,
                        );
                    }
                    y += wrapped.len() as f32 * line_h;
                    if *level <= 2 {
                        y += 4.0;
                        canvas.add_line(
                            MARGIN,
                            y,
                            canvas.budget_width() - MARGIN,
                            y,
                            palette.border,
                            1.0,
                        );
                    }
                }
                Block::Bullet { depth, text } => {
                    let x = MARGIN + (*depth).min(4) as f32 * BULLET_INDENT_PX;
                    let text_x = x + Font::Sans.char_advance(BODY_SIZE) * 2.0;
                    canvas.add_text(
                        x,
                        y + BODY_SIZE,
                        "\u{2022}",
                        Font::Sans,
                        BODY_SIZE,
                        palette.accent_a,
                        false,
                        false,
                    );
                    let max_chars = max_chars_for(&canvas, text_x, Font::Sans, BODY_SIZE);
                    let wrapped = wrap_by_width(text, max_chars);
                    for (wi, wline) in wrapped.iter().enumerate() {
                        let baseline = y + wi as f32 * body_line_h + BODY_SIZE;
                        canvas.add_text(
                            text_x,
                            baseline,
                            wline,
                            Font::Sans,
                            BODY_SIZE,
                            palette.foreground,
                            false,
                            false,
                        );
                    }
                    y += wrapped.len().max(1) as f32 * body_line_h;
                }
                Block::Ordered {
                    depth,
                    marker,
                    text,
                } => {
                    let x = MARGIN + (*depth).min(4) as f32 * BULLET_INDENT_PX;
                    let text_x = x
                        + (marker.chars().count() as f32 + 1.0)
                            * Font::Sans.char_advance(BODY_SIZE);
                    canvas.add_text(
                        x,
                        y + BODY_SIZE,
                        marker,
                        Font::Sans,
                        BODY_SIZE,
                        palette.accent_a,
                        false,
                        false,
                    );
                    let max_chars = max_chars_for(&canvas, text_x, Font::Sans, BODY_SIZE);
                    let wrapped = wrap_by_width(text, max_chars);
                    for (wi, wline) in wrapped.iter().enumerate() {
                        let baseline = y + wi as f32 * body_line_h + BODY_SIZE;
                        canvas.add_text(
                            text_x,
                            baseline,
                            wline,
                            Font::Sans,
                            BODY_SIZE,
                            palette.foreground,
                            false,
                            false,
                        );
                    }
                    y += wrapped.len().max(1) as f32 * body_line_h;
                }
                Block::Code { language, source } => {
                    y = code::render_code_block(
                        &mut canvas,
                        source,
                        language.as_deref(),
                        opts.theme,
                        &palette,
                        MARGIN,
                        y,
                        code::CODE_FONT_SIZE,
                        true,
                    )?;
                }
                Block::Table(table) => {
                    y = table::draw_table(
                        &mut canvas,
                        table,
                        MARGIN,
                        y,
                        table::TABLE_FONT_SIZE,
                        &palette,
                    );
                }
                Block::Paragraph(text) => {
                    let max_chars = max_chars_for(&canvas, MARGIN, Font::Sans, BODY_SIZE);
                    let wrapped = wrap_by_width(text, max_chars);
                    for (wi, wline) in wrapped.iter().enumerate() {
                        let baseline = y + wi as f32 * body_line_h + BODY_SIZE;
                        canvas.add_text(
                            MARGIN,
                            baseline,
                            wline,
                            Font::Sans,
                            BODY_SIZE,
                            palette.foreground,
                            false,
                            false,
                        );
                    }
                    y += wrapped.len().max(1) as f32 * body_line_h;
                }
            }
        }

        let svg = canvas.finish(MARGIN, &palette);
        let mut asset = crate::canvas::rasterize(&svg, opts)?;
        asset.alt_text = Some(format!(
            "Rendered markdown document, {} block(s)",
            blocks.len()
        ));
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use morph_core::traits::Renderer;

    fn sample() -> DetectedContent {
        let raw = r#"# Title

Some intro paragraph that talks about the gateway and how it renders
content for models that read images better than raw text.

## Section

- first point
- second point with a bit more text so it might wrap onto another line

1. step one
2. step two

| Col A | Col B |
|---|---|
| 1 | 2 |

```rust
fn main() {
    println!("hi");
}
```
"#
        .to_string();
        DetectedContent {
            kind: ContentKind::Markdown,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }

    #[test]
    fn parses_expected_block_sequence() {
        let blocks = parse_blocks(&sample().raw);
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Heading { level: 2, .. }));
        assert!(blocks.iter().any(|b| matches!(b, Block::Bullet { .. })));
        assert!(blocks.iter().any(|b| matches!(b, Block::Ordered { .. })));
        assert!(blocks.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::Code { .. })));
    }

    #[test]
    fn snapshot_markdown_document_svg() {
        let renderer = MarkdownRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&sample(), &opts)
            .expect("render should succeed");
        let svg = String::from_utf8(asset.bytes).expect("svg should be utf8");
        assert_snapshot!(svg);
    }

    #[test]
    fn rasterize_png_has_magic_bytes_and_size() {
        let renderer = MarkdownRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Png,
            theme: Theme::Light,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&sample(), &opts)
            .expect("render should succeed");
        assert!(asset.bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(asset.width > 0 && asset.height > 0);
    }

    #[test]
    fn supports_markdown_and_plain_text() {
        let renderer = MarkdownRenderer::new();
        assert!(renderer.supports(ContentKind::Markdown));
        assert!(renderer.supports(ContentKind::PlainText));
        assert!(!renderer.supports(ContentKind::Json));
    }
}
