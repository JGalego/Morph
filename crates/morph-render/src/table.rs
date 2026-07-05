//! Table rendering: parses markdown pipe-tables (falling back to naive CSV)
//! into rows/columns, then draws a grid with a distinct header background
//! and alternating row stripes.

use morph_core::prelude::*;

use crate::canvas::{Canvas, Font, Palette, LINE_HEIGHT_RATIO, MARGIN};

/// Default font size for table cells (px), reused by `markdown.rs` for
/// pipe tables embedded in a document.
pub(crate) const TABLE_FONT_SIZE: f32 = 14.0;
/// Columns wider than this (in characters) are truncated with an ellipsis
/// rather than growing the table indefinitely.
const MAX_COL_CHARS: usize = 28;
const MIN_COL_CHARS: usize = 3;

/// A parsed table: uniform-ish rows under a header row. Rows may be shorter
/// than `headers` (ragged input); missing trailing cells render empty.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn split_pipe_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    trimmed.split('|').map(|c| c.trim().to_string()).collect()
}

/// A markdown table separator row looks like `| --- | :---: | ---: |`: every
/// cell is non-empty and made up only of `-`/`:`, with at least one `-`.
fn is_separator_row(line: &str) -> bool {
    let cells = split_pipe_row(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| matches!(ch, '-' | ':'))
        })
}

/// Parses GitHub-flavored-markdown pipe-table syntax:
/// ```text
/// | a | b |
/// |---|---|
/// | 1 | 2 |
/// ```
/// Returns `None` if `text` doesn't look like a pipe table at all (no
/// header/separator pair), so callers can fall back to CSV.
pub(crate) fn parse_pipe_table(text: &str) -> Option<Table> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return None;
    }
    let headers = split_pipe_row(lines[0]);
    if headers.is_empty() || !is_separator_row(lines[1]) {
        return None;
    }
    let rows = lines[2..].iter().map(|l| split_pipe_row(l)).collect();
    Some(Table { headers, rows })
}

fn split_csv_row(line: &str) -> Vec<String> {
    // Deliberately naive: no quoted-comma/escaping support. This is only a
    // fallback for when a segment doesn't parse as a pipe table at all.
    line.split(',')
        .map(|c| c.trim().trim_matches('"').to_string())
        .collect()
}

/// Fallback parser for plain CSV, used when [`parse_pipe_table`] finds no
/// markdown table syntax in the segment.
pub(crate) fn parse_csv(text: &str) -> Table {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Table {
            headers: Vec::new(),
            rows: Vec::new(),
        };
    }
    let headers = split_csv_row(lines[0]);
    let rows = lines[1..].iter().map(|l| split_csv_row(l)).collect();
    Table { headers, rows }
}

fn truncate_cell(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "\u{2026}".to_string();
    }
    let truncated: String = s.chars().take(max_chars - 1).collect();
    format!("{truncated}\u{2026}")
}

fn column_widths(table: &Table) -> Vec<usize> {
    let n_cols = table
        .headers
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    let mut widths = vec![MIN_COL_CHARS; n_cols];
    for (i, h) in table.headers.iter().enumerate() {
        widths[i] = widths[i].max(h.chars().count());
    }
    for row in &table.rows {
        for (i, c) in row.iter().enumerate() {
            if i < n_cols {
                widths[i] = widths[i].max(c.chars().count());
            }
        }
    }
    for w in &mut widths {
        *w = (*w).clamp(MIN_COL_CHARS, MAX_COL_CHARS);
    }
    widths
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    canvas: &mut Canvas,
    cells: &[String],
    col_chars: &[usize],
    col_px: &[f32],
    x0: f32,
    y: f32,
    row_h: f32,
    font_size: f32,
    pad_x: f32,
    color: &str,
    bold: bool,
) {
    let baseline = y + row_h / 2.0 + font_size * 0.35;
    let mut cx = x0;
    for i in 0..col_px.len() {
        let raw = cells.get(i).map(String::as_str).unwrap_or("");
        let text = truncate_cell(raw, col_chars[i]);
        canvas.add_text(
            cx + pad_x,
            baseline,
            &text,
            Font::Mono,
            font_size,
            color,
            bold,
            false,
        );
        cx += col_px[i];
    }
}

/// Draws `table` as a grid with a header background and alternating row
/// stripes, starting at `(x0, y0)`. Returns the y coordinate immediately
/// below the table. Shared by [`TableRenderer`] and `markdown.rs`'s
/// pipe-table handling.
pub(crate) fn draw_table(
    canvas: &mut Canvas,
    table: &Table,
    x0: f32,
    y0: f32,
    font_size: f32,
    palette: &Palette,
) -> f32 {
    let col_chars = column_widths(table);
    if col_chars.is_empty() {
        return y0;
    }
    let line_h = font_size * LINE_HEIGHT_RATIO;
    let pad_x = font_size * 0.6;
    let char_w = Font::Mono.char_advance(font_size);
    let col_px: Vec<f32> = col_chars
        .iter()
        .map(|w| *w as f32 * char_w + pad_x * 2.0)
        .collect();
    let table_w: f32 = col_px.iter().sum();
    let row_h = line_h + pad_x * 0.5;

    let mut y = y0;
    canvas.add_rect(x0, y, table_w, row_h, palette.header_bg, 1.0);
    draw_row(
        canvas,
        &table.headers,
        &col_chars,
        &col_px,
        x0,
        y,
        row_h,
        font_size,
        pad_x,
        palette.foreground,
        true,
    );
    y += row_h;

    for (ri, row) in table.rows.iter().enumerate() {
        if ri % 2 == 1 {
            canvas.add_rect(x0, y, table_w, row_h, palette.row_alt_bg, 1.0);
        }
        draw_row(
            canvas,
            row,
            &col_chars,
            &col_px,
            x0,
            y,
            row_h,
            font_size,
            pad_x,
            palette.foreground,
            false,
        );
        y += row_h;
    }

    canvas.add_line(x0, y0, x0 + table_w, y0, palette.border, 1.0);
    canvas.add_line(
        x0,
        y0 + row_h,
        x0 + table_w,
        y0 + row_h,
        palette.border,
        1.0,
    );
    canvas.add_line(x0, y, x0 + table_w, y, palette.border, 1.0);
    canvas.add_line(x0, y0, x0, y, palette.border, 1.0);
    canvas.add_line(x0 + table_w, y0, x0 + table_w, y, palette.border, 1.0);
    let mut cx = x0;
    for w in &col_px[..col_px.len().saturating_sub(1)] {
        cx += *w;
        canvas.add_line(cx, y0, cx, y, palette.border, 1.0);
    }

    y
}

/// Renders `ContentKind::Table` segments: markdown pipe-table syntax first,
/// CSV as a fallback for anything that doesn't parse as one.
pub struct TableRenderer;

impl TableRenderer {
    pub fn new() -> Self {
        TableRenderer
    }
}

impl Default for TableRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl morph_core::traits::Renderer for TableRenderer {
    fn name(&self) -> &str {
        "table"
    }

    fn supports(&self, kind: ContentKind) -> bool {
        kind == ContentKind::Table
    }

    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset> {
        let table = parse_pipe_table(&content.raw).unwrap_or_else(|| parse_csv(&content.raw));
        if table.headers.is_empty() {
            return Err(GatewayError::Render(
                "no tabular data found in segment".to_string(),
            ));
        }

        let palette = Palette::for_theme(opts.theme);
        let mut canvas = Canvas::new(opts.max_width_px as f32);
        draw_table(
            &mut canvas,
            &table,
            MARGIN,
            MARGIN,
            TABLE_FONT_SIZE,
            &palette,
        );

        let svg = canvas.finish(MARGIN, &palette);
        let mut asset = crate::canvas::rasterize(&svg, opts)?;
        asset.alt_text = Some(format!(
            "Table with {} column(s) and {} row(s)",
            table.headers.len(),
            table.rows.len()
        ));
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use morph_core::traits::Renderer;

    fn pipe_sample() -> DetectedContent {
        let raw = "| Name | Role | Years |\n|---|---|---|\n| Ada | Engineer | 7 |\n| Grace | Admiral | 44 |\n"
            .to_string();
        DetectedContent {
            kind: ContentKind::Table,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }

    #[test]
    fn parses_pipe_table() {
        let table = parse_pipe_table(&pipe_sample().raw).expect("should parse");
        assert_eq!(table.headers, vec!["Name", "Role", "Years"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0], vec!["Ada", "Engineer", "7"]);
    }

    #[test]
    fn falls_back_to_csv_when_not_a_pipe_table() {
        let csv = "a,b,c\n1,2,3\n";
        assert!(parse_pipe_table(csv).is_none());
        let table = parse_csv(csv);
        assert_eq!(table.headers, vec!["a", "b", "c"]);
        assert_eq!(table.rows, vec![vec!["1", "2", "3"]]);
    }

    #[test]
    fn truncates_overlong_cells() {
        assert_eq!(truncate_cell("short", 10), "short");
        assert_eq!(truncate_cell("a very long cell value", 6), "a ver\u{2026}");
    }

    #[test]
    fn snapshot_table_svg() {
        let renderer = TableRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Svg,
            theme: Theme::Dark,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&pipe_sample(), &opts)
            .expect("render should succeed");
        let svg = String::from_utf8(asset.bytes).expect("svg should be utf8");
        assert_snapshot!(svg);
    }

    #[test]
    fn rasterize_png_has_magic_bytes_and_size() {
        let renderer = TableRenderer::new();
        let opts = RenderOptions {
            format: RasterFormat::Png,
            theme: Theme::Light,
            ..RenderOptions::default()
        };
        let asset = renderer
            .render(&pipe_sample(), &opts)
            .expect("render should succeed");
        assert!(asset.bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert!(asset.width > 0 && asset.height > 0);
    }

    #[test]
    fn supports_only_table_kind() {
        let renderer = TableRenderer::new();
        assert!(renderer.supports(ContentKind::Table));
        assert!(!renderer.supports(ContentKind::Csv));
    }
}
