//! JSON tree rendering: keys, string/number/bool/null values, and
//! punctuation are each colored distinctly, and a faint vertical guide line
//! marks each nesting level so deep structures stay readable.

use morph_core::prelude::*;
use serde_json::Value;

use crate::canvas::{Canvas, Font, Palette, LINE_HEIGHT_RATIO, MARGIN};

const JSON_FONT_SIZE: f32 = 14.0;
const INDENT_CHARS: f32 = 2.0;

/// Layout constants threaded through the recursive walk. Grouped into one
/// `Copy` struct so `render_value` doesn't need six separate parameters
/// re-passed at every recursion level.
#[derive(Clone, Copy)]
struct Layout<'a> {
    palette: &'a Palette,
    font_size: f32,
    line_h: f32,
    indent_w: f32,
    x0: f32,
}

/// Draws `text` at `(*x, y)` and advances `*x` past it. Centralizes the
/// "draw then advance the pen" bookkeeping every token (key, colon, value,
/// punctuation) needs.
fn put(canvas: &mut Canvas, x: &mut f32, y: f32, text: &str, layout: Layout<'_>, color: &str) {
    if text.is_empty() {
        return;
    }
    canvas.add_text(
        *x,
        y,
        text,
        Font::Mono,
        layout.font_size,
        color,
        false,
        false,
    );
    *x += text.chars().count() as f32 * Font::Mono.char_advance(layout.font_size);
}

fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn scalar_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

fn scalar_color<'p>(v: &Value, palette: &'p Palette) -> &'p str {
    match v {
        Value::String(_) => palette.accent_b,
        Value::Number(_) | Value::Bool(_) | Value::Null => palette.accent_c,
        Value::Object(_) | Value::Array(_) => palette.foreground,
    }
}

/// Draws a faint vertical indent guide for one nesting level, spanning the
/// rows occupied by that level's children.
fn draw_guide(canvas: &mut Canvas, layout: Layout<'_>, depth: usize, top: f32, bottom: f32) {
    if bottom <= top {
        return;
    }
    let x = layout.x0 + (depth as f32 + 1.0) * layout.indent_w - layout.indent_w * 0.5;
    let inset = layout.line_h * 0.15;
    canvas.add_line(x, top + inset, x, bottom - inset, layout.palette.muted, 1.0);
}

/// Recursively draws one JSON value (with an optional preceding `"key": `)
/// starting at `*y`, advancing `*y` by one line per row emitted.
fn render_value(
    canvas: &mut Canvas,
    layout: Layout<'_>,
    key: Option<&str>,
    value: &Value,
    depth: usize,
    trailing_comma: bool,
    y: &mut f32,
) {
    let mut x = layout.x0 + depth as f32 * layout.indent_w;
    let baseline = *y + layout.font_size;
    if let Some(k) = key {
        put(
            canvas,
            &mut x,
            baseline,
            &json_quote(k),
            layout,
            layout.palette.accent_a,
        );
        put(
            canvas,
            &mut x,
            baseline,
            ": ",
            layout,
            layout.palette.accent_e,
        );
    }

    match value {
        Value::Object(map) if !map.is_empty() => {
            put(
                canvas,
                &mut x,
                baseline,
                "{",
                layout,
                layout.palette.accent_e,
            );
            let guide_top = *y;
            *y += layout.line_h;
            let n = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                render_value(canvas, layout, Some(k.as_str()), v, depth + 1, i + 1 < n, y);
            }
            let guide_bottom = *y;
            let mut cx = layout.x0 + depth as f32 * layout.indent_w;
            let close_baseline = *y + layout.font_size;
            put(
                canvas,
                &mut cx,
                close_baseline,
                "}",
                layout,
                layout.palette.accent_e,
            );
            if trailing_comma {
                put(
                    canvas,
                    &mut cx,
                    close_baseline,
                    ",",
                    layout,
                    layout.palette.accent_e,
                );
            }
            draw_guide(canvas, layout, depth, guide_top, guide_bottom);
            *y += layout.line_h;
        }
        Value::Array(items) if !items.is_empty() => {
            put(
                canvas,
                &mut x,
                baseline,
                "[",
                layout,
                layout.palette.accent_e,
            );
            let guide_top = *y;
            *y += layout.line_h;
            let n = items.len();
            for (i, v) in items.iter().enumerate() {
                render_value(canvas, layout, None, v, depth + 1, i + 1 < n, y);
            }
            let guide_bottom = *y;
            let mut cx = layout.x0 + depth as f32 * layout.indent_w;
            let close_baseline = *y + layout.font_size;
            put(
                canvas,
                &mut cx,
                close_baseline,
                "]",
                layout,
                layout.palette.accent_e,
            );
            if trailing_comma {
                put(
                    canvas,
                    &mut cx,
                    close_baseline,
                    ",",
                    layout,
                    layout.palette.accent_e,
                );
            }
            draw_guide(canvas, layout, depth, guide_top, guide_bottom);
            *y += layout.line_h;
        }
        Value::Object(_) => {
            put(
                canvas,
                &mut x,
                baseline,
                "{}",
                layout,
                layout.palette.accent_e,
            );
            if trailing_comma {
                put(
                    canvas,
                    &mut x,
                    baseline,
                    ",",
                    layout,
                    layout.palette.accent_e,
                );
            }
            *y += layout.line_h;
        }
        Value::Array(_) => {
            put(
                canvas,
                &mut x,
                baseline,
                "[]",
                layout,
                layout.palette.accent_e,
            );
            if trailing_comma {
                put(
                    canvas,
                    &mut x,
                    baseline,
                    ",",
                    layout,
                    layout.palette.accent_e,
                );
            }
            *y += layout.line_h;
        }
        scalar => {
            put(
                canvas,
                &mut x,
                baseline,
                &scalar_text(scalar),
                layout,
                scalar_color(scalar, layout.palette),
            );
            if trailing_comma {
                put(
                    canvas,
                    &mut x,
                    baseline,
                    ",",
                    layout,
                    layout.palette.accent_e,
                );
            }
            *y += layout.line_h;
        }
    }
}

/// Renders `ContentKind::Json` segments as an indented, colorized tree.
pub struct JsonRenderer;

impl JsonRenderer {
    pub fn new() -> Self {
        JsonRenderer
    }
}

impl Default for JsonRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl morph_core::traits::Renderer for JsonRenderer {
    fn name(&self) -> &str {
        "json"
    }

    fn supports(&self, kind: ContentKind) -> bool {
        kind == ContentKind::Json
    }

    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset> {
        let value: Value = serde_json::from_str(&content.raw)
            .map_err(|e| GatewayError::Render(format!("invalid JSON: {e}")))?;

        let palette = Palette::for_theme(opts.theme);
        let mut canvas = Canvas::new(opts.max_width_px as f32);
        let layout = Layout {
            palette: &palette,
            font_size: JSON_FONT_SIZE,
            line_h: JSON_FONT_SIZE * LINE_HEIGHT_RATIO,
            indent_w: INDENT_CHARS * Font::Mono.char_advance(JSON_FONT_SIZE),
            x0: MARGIN,
        };
        let mut y = MARGIN;
        render_value(&mut canvas, layout, None, &value, 0, false, &mut y);

        let svg = canvas.finish(MARGIN, &palette);
        let mut asset = crate::canvas::rasterize(&svg, opts)?;
        asset.alt_text = Some(format!(
            "JSON tree, {} top-level {}",
            match &value {
                Value::Object(m) => m.len(),
                Value::Array(a) => a.len(),
                _ => 1,
            },
            match &value {
                Value::Object(_) => "key(s)",
                Value::Array(_) => "element(s)",
                _ => "value",
            },
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
        let raw = r#"{
  "name": "morph",
  "version": 1,
  "active": true,
  "tags": ["gateway", "llm"],
  "meta": {"owner": null}
}"#
        .to_string();
        DetectedContent {
            kind: ContentKind::Json,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }

    #[test]
    fn snapshot_json_tree_svg() {
        let renderer = JsonRenderer::new();
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
        let renderer = JsonRenderer::new();
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
    fn rejects_invalid_json() {
        let renderer = JsonRenderer::new();
        let content = DetectedContent::plain("{not valid json");
        let opts = RenderOptions::default();
        assert!(renderer.render(&content, &opts).is_err());
    }

    #[test]
    fn supports_only_json_kind() {
        let renderer = JsonRenderer::new();
        assert!(renderer.supports(ContentKind::Json));
        assert!(!renderer.supports(ContentKind::Yaml));
    }
}
