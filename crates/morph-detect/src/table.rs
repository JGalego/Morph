//! Table detection: Markdown pipe-tables and simple ASCII-box tables.
//!
//! Both shapes are common in tool output and rendered documentation, and
//! both carry an unambiguous column/row count that the representation
//! planner uses (rule e) to decide whether a table is "big enough" to be
//! worth a supplementary image.

use std::sync::LazyLock;

use regex::Regex;

static PIPE_ROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*\|.+\|[ \t]*$").expect("valid regex"));
static PIPE_SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*\|?[ \t]*:?-{2,}:?[ \t]*(\|[ \t]*:?-{2,}:?[ \t]*)+\|?[ \t]*$")
        .expect("valid regex")
});
static BOX_BORDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\+[-+]+\+$").expect("valid regex"));

/// Result of a table heuristic scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableDetection {
    pub confidence: f32,
    pub columns: usize,
    pub rows: usize,
}

/// Tries the ASCII box-drawing table shape first: its `+---+---+` border is
/// a more specific, less ambiguous signal than bare pipe rows, and a box
/// table's content rows would otherwise also satisfy the pipe-table check
/// (every row starts and ends with `|`), causing it to be miscounted as a
/// headerless pipe table. Falls back to the far more common Markdown
/// pipe-table shape when no box border is present.
pub fn detect(text: &str) -> TableDetection {
    if let Some(d) = detect_box_table(text) {
        return d;
    }
    if let Some(d) = detect_pipe_table(text) {
        return d;
    }
    TableDetection {
        confidence: 0.0,
        columns: 0,
        rows: 0,
    }
}

fn detect_pipe_table(text: &str) -> Option<TableDetection> {
    let pipe_rows: Vec<&str> = text.lines().filter(|l| PIPE_ROW_RE.is_match(l)).collect();
    if pipe_rows.len() < 2 {
        return None;
    }

    let has_separator = text.lines().any(|l| PIPE_SEPARATOR_RE.is_match(l));
    let columns = count_pipe_columns(pipe_rows[0]);
    if columns < 2 {
        return None;
    }
    // The header row (and, when present, the `|---|---|` separator row) are
    // not data rows.
    let rows = if has_separator {
        pipe_rows.len().saturating_sub(2)
    } else {
        pipe_rows.len().saturating_sub(1)
    };

    let confidence: f32 = if has_separator { 0.85 } else { 0.55 };
    Some(TableDetection {
        confidence: confidence.min(0.97),
        columns,
        rows,
    })
}

fn detect_box_table(text: &str) -> Option<TableDetection> {
    let border_lines = text
        .lines()
        .filter(|l| BOX_BORDER_RE.is_match(l.trim()))
        .count();
    if border_lines < 2 {
        return None;
    }

    let content_lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('|') && !BOX_BORDER_RE.is_match(t)
        })
        .collect();
    if content_lines.is_empty() {
        return None;
    }

    let columns = count_pipe_columns(content_lines[0]);
    if columns < 2 {
        return None;
    }
    let rows = content_lines.len();
    Some(TableDetection {
        confidence: 0.8,
        columns,
        rows,
    })
}

/// Counts columns in one pipe-delimited row by stripping the leading/trailing
/// pipe and splitting on `|`.
fn count_pipe_columns(row: &str) -> usize {
    let trimmed = row.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    if inner.trim().is_empty() {
        0
    } else {
        inner.split('|').count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_pipe_table_with_separator_detected() {
        let text =
            "| Name | Age | City |\n| --- | --- | --- |\n| Alice | 30 | NYC |\n| Bob | 25 | LA |\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.columns, 3);
        assert_eq!(d.rows, 2);
    }

    #[test]
    fn pipe_table_without_separator_detected() {
        let text = "| id | value |\n| 1 | foo |\n| 2 | bar |\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.columns, 2);
    }

    #[test]
    fn ascii_box_table_detected() {
        let text = "+------+-----+\n| Name | Age |\n+------+-----+\n| Ann  | 22  |\n| Bo   | 31  |\n+------+-----+\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.columns, 2);
        assert_eq!(d.rows, 3);
    }

    #[test]
    fn single_pipe_line_is_not_a_table() {
        let text = "The result is a | b, which is not really a table row.";
        let d = detect(text);
        assert_eq!(d.confidence, 0.0);
    }
}
