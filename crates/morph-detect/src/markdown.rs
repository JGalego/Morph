//! Markdown detection.
//!
//! Markdown has no formal grammar to parse against (unlike JSON/XML), so the
//! signal is purely structural density: how many of the handful of
//! unambiguous Markdown markers (ATX headers, list bullets, pipe tables,
//! fenced code blocks, inline links) show up per line. A prose paragraph
//! that happens to contain a literal `-` or `*` scores near zero; a README
//! full of headers and bullets scores high.

use std::sync::LazyLock;

use regex::Regex;

static HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ ]{0,3}#{1,6}[ \t]+\S").expect("valid regex"));
static LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ ]{0,3}(?:[-*+][ \t]+\S|\d+\.[ \t]+\S)").expect("valid regex")
});
static TABLE_ROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ ]{0,3}\|.+\|[ \t]*$").expect("valid regex"));
static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ ]{0,3}(?:```|~~~)").expect("valid regex"));
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[^\]\n]+\]\([^)\n]+\)").expect("valid regex"));

/// Result of scanning one text segment for Markdown markers.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownDetection {
    pub confidence: f32,
}

/// Scores `text` for how strongly it resembles Markdown prose.
///
/// The formula rewards both *density* (markers per line) and *variety*
/// (distinct marker kinds present), because a document that mixes headers
/// with a fenced code block is much more likely to be genuine Markdown than
/// one with the same number of hits from a single repeated marker (e.g. ten
/// dashes that are actually a table of contents-free plain list).
pub fn detect(text: &str) -> MarkdownDetection {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return MarkdownDetection { confidence: 0.0 };
    }

    let line_count = text.lines().count().max(1) as f32;

    let headers = HEADER_RE.find_iter(text).count();
    let lists = LIST_RE.find_iter(text).count();
    let table_rows = TABLE_ROW_RE.find_iter(text).count();
    let fences = FENCE_RE.find_iter(text).count();
    let links = LINK_RE.find_iter(text).count();

    let marker_hits = headers + lists + table_rows + fences + links;
    if marker_hits == 0 {
        return MarkdownDetection { confidence: 0.0 };
    }
    let distinct_kinds = [headers, lists, table_rows, fences, links]
        .iter()
        .filter(|&&c| c > 0)
        .count() as f32;

    let density = marker_hits as f32 / line_count;
    let confidence = (density * 1.5 + distinct_kinds * 0.1).min(1.0);

    MarkdownDetection { confidence }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_headers_lists_and_fences() {
        let text = "# Title\n\nSome intro text.\n\n- item one\n- item two\n- item three\n\n```rust\nfn main() {}\n```\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn detects_pipe_table_and_links() {
        let text = "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n\nSee [the docs](https://example.com) for more.\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn detects_ordered_list_heavy_document() {
        let text = "## Steps\n1. Clone the repo\n2. Install dependencies\n3. Run the tests\n4. Open a PR\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn plain_prose_scores_low() {
        let text = "The quick brown fox jumps over the lazy dog. It was a calm afternoon, \
                     and nothing of note happened besides the usual chatter in the office.";
        let d = detect(text);
        assert!(d.confidence < 0.35, "confidence was {}", d.confidence);
    }
}
