//! Detectors for structured/serialization formats: JSON, YAML, XML/HTML,
//! CSV, and SQL. Unlike `markdown`/`code`, several of these can lean on an
//! actual parser (`serde_json`) rather than pure heuristics, which makes
//! them much more precise.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Result of attempting to parse a segment as JSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonDetection {
    pub confidence: f32,
    /// Nesting depth of the parsed value; 0 for scalars or parse failures.
    pub depth: usize,
}

/// Tries to parse `text` as JSON. Only a top-level object or array counts as
/// "JSON content" for classification purposes — a bare scalar like `"42"` or
/// `"true"` parses fine but isn't meaningfully different from plain text.
pub fn detect_json(text: &str) -> JsonDetection {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return JsonDetection {
            confidence: 0.0,
            depth: 0,
        };
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => match &value {
            Value::Object(_) | Value::Array(_) => {
                let depth = json_depth(&value);
                let confidence = if depth >= 2 { 0.95 } else { 0.75 };
                JsonDetection { confidence, depth }
            }
            _ => JsonDetection {
                confidence: 0.1,
                depth: 0,
            },
        },
        Err(_) => JsonDetection {
            confidence: 0.0,
            depth: 0,
        },
    }
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
        Value::Array(arr) => 1 + arr.iter().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

static YAML_KV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^( *)([A-Za-z0-9_.\-]+):(?:[ \t]|$)").expect("valid regex"));
static YAML_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^( *)-[ \t]+\S").expect("valid regex"));

/// Result of a YAML heuristic scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct YamlDetection {
    pub confidence: f32,
    /// Rough nesting depth, estimated from the deepest indentation seen
    /// (YAML has no delimiters to parse depth from exactly without a full
    /// parser, which this crate deliberately avoids pulling in).
    pub depth: usize,
}

/// Heuristic YAML detector: `key: value` lines, `---` document markers, and
/// `- item` list lines are the distinctive YAML markers; tabs are illegal in
/// YAML indentation and are treated as a hard disqualifier.
pub fn detect_yaml(text: &str) -> YamlDetection {
    if text.contains('\t') {
        return YamlDetection {
            confidence: 0.0,
            depth: 0,
        };
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return YamlDetection {
            confidence: 0.0,
            depth: 0,
        };
    }

    let non_empty_lines = text.lines().filter(|l| !l.trim().is_empty()).count().max(1);
    let has_doc_marker = text.lines().any(|l| l.trim() == "---");

    let kv_indents: Vec<usize> = YAML_KV_RE.captures_iter(text).map(|c| c[1].len()).collect();
    let list_indents: Vec<usize> = YAML_LIST_RE
        .captures_iter(text)
        .map(|c| c[1].len())
        .collect();
    let hit_count = kv_indents.len() + list_indents.len();

    // Require at least two structural hits (or an explicit doc marker) so a
    // single stray "Note: see below" sentence in plain prose doesn't trigger
    // a false positive.
    if hit_count < 2 && !has_doc_marker {
        return YamlDetection {
            confidence: 0.0,
            depth: 0,
        };
    }

    let density = (hit_count as f32 / non_empty_lines as f32).min(1.0);
    let mut confidence = density * 0.8;
    if has_doc_marker {
        confidence += 0.2;
    }
    confidence = confidence.min(1.0);

    let max_indent = kv_indents
        .iter()
        .chain(list_indents.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let depth = if hit_count == 0 {
        0
    } else {
        1 + max_indent / 2
    };

    YamlDetection { confidence, depth }
}

// ---------------------------------------------------------------------------
// XML / HTML
// ---------------------------------------------------------------------------

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"</?[A-Za-z][A-Za-z0-9:_-]*(?:\s[^<>]*)?/?>").expect("valid regex")
});
static OPEN_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Za-z][A-Za-z0-9:_-]*)(?:\s[^<>]*)?>").expect("valid regex"));
static CLOSE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</([A-Za-z][A-Za-z0-9:_-]*)>").expect("valid regex"));

const HTML_MARKERS: &[&str] = &[
    "<!doctype html",
    "<html",
    "<head",
    "<body",
    "<div",
    "<span",
    "<a href",
];

/// Result of an XML/HTML heuristic scan. `is_html` distinguishes the two
/// (both share the same tag-balancing heuristic) via common HTML-only tags.
#[derive(Debug, Clone, Copy, Default)]
pub struct XmlDetection {
    pub confidence: f32,
    pub is_html: bool,
}

/// Heuristic XML/HTML detector: an `<?xml` prolog is a near-certain signal;
/// otherwise, score by tag density and how well open/close tags balance.
pub fn detect_xml(text: &str) -> XmlDetection {
    let trimmed = text.trim_start();
    let has_prolog = trimmed.starts_with("<?xml");

    let tag_matches = TAG_RE.find_iter(text).count();
    if tag_matches == 0 && !has_prolog {
        return XmlDetection {
            confidence: 0.0,
            is_html: false,
        };
    }

    let open_tags: Vec<String> = OPEN_TAG_RE
        .captures_iter(text)
        .map(|c| c[1].to_lowercase())
        .collect();
    let close_tags: Vec<String> = CLOSE_TAG_RE
        .captures_iter(text)
        .map(|c| c[1].to_lowercase())
        .collect();

    let balance_score = if open_tags.is_empty() && close_tags.is_empty() {
        0.0
    } else {
        let mut counts: HashMap<String, i32> = HashMap::new();
        for t in &open_tags {
            *counts.entry(t.clone()).or_insert(0) += 1;
        }
        for t in &close_tags {
            *counts.entry(t.clone()).or_insert(0) -= 1;
        }
        let unmatched: i32 = counts.values().map(|v| v.abs()).sum();
        let total = (open_tags.len() + close_tags.len()) as i32;
        if total == 0 {
            0.0
        } else {
            (1.0 - (unmatched as f32 / total as f32)).max(0.0)
        }
    };

    let line_count = text.lines().count().max(1) as f32;
    let tag_density = (tag_matches as f32 / line_count).min(1.5);

    let lower = text.to_lowercase();
    let is_html = HTML_MARKERS.iter().any(|m| lower.contains(m));

    let mut confidence = (tag_density * 0.4 + balance_score * 0.5).min(1.0);
    if has_prolog {
        confidence = confidence.max(0.9);
    }
    if is_html {
        confidence = confidence.max(0.5);
    }

    XmlDetection {
        confidence: confidence.min(1.0),
        is_html,
    }
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// Result of a CSV heuristic scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct CsvDetection {
    pub confidence: f32,
    pub columns: usize,
    pub rows: usize,
}

const CSV_DELIMITERS: [char; 3] = [',', ';', '\t'];

/// Heuristic CSV detector: looks for a delimiter whose per-line count is
/// perfectly consistent across every non-empty line — the defining property
/// of CSV-shaped data, as opposed to prose that merely contains commas.
pub fn detect_csv(text: &str) -> CsvDetection {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return CsvDetection {
            confidence: 0.0,
            columns: 0,
            rows: 0,
        };
    }

    for delim in CSV_DELIMITERS {
        let first_count = lines[0].matches(delim).count();
        if first_count == 0 {
            continue;
        }
        let all_consistent = lines
            .iter()
            .all(|l| l.matches(delim).count() == first_count);
        if all_consistent {
            let columns = first_count + 1;
            if columns < 2 {
                continue;
            }
            let confidence = (0.55 + 0.05 * columns.min(6) as f32).min(0.95);
            return CsvDetection {
                confidence,
                columns,
                rows: lines.len(),
            };
        }
    }

    CsvDetection {
        confidence: 0.0,
        columns: 0,
        rows: 0,
    }
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

static SQL_STATEMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM|CREATE\s+TABLE|CREATE\s+INDEX|ALTER\s+TABLE|DROP\s+TABLE|WITH)\b")
        .expect("valid regex")
});
static SQL_CLAUSE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(FROM|WHERE|JOIN|GROUP BY|ORDER BY|VALUES|SET|INTO)\b").expect("valid regex")
});

/// Result of a SQL heuristic scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlDetection {
    pub confidence: f32,
}

/// Heuristic SQL detector: requires at least one statement keyword
/// (SELECT/INSERT/UPDATE/DELETE/CREATE TABLE/...) at a statement boundary
/// (start of line), so prose that merely uses the word "select" in a
/// sentence doesn't false-positive.
pub fn detect_sql(text: &str) -> SqlDetection {
    let statement_hits = SQL_STATEMENT_RE.find_iter(text).count();
    if statement_hits == 0 {
        return SqlDetection { confidence: 0.0 };
    }
    let clause_hits = SQL_CLAUSE_RE.find_iter(text).count();
    let semicolon_terminated = text.contains(';');

    let mut confidence =
        0.55 + (statement_hits.min(3) as f32) * 0.05 + (clause_hits.min(4) as f32) * 0.04;
    if semicolon_terminated {
        confidence += 0.05;
    }
    SqlDetection {
        confidence: confidence.min(0.95),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- JSON ---

    #[test]
    fn json_object_detected_with_depth() {
        let text = r#"{"user": {"name": "Ada", "roles": ["admin", "editor"]}}"#;
        let d = detect_json(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert!(d.depth >= 2, "depth was {}", d.depth);
    }

    #[test]
    fn json_array_detected() {
        let text = r#"[1, 2, 3, {"a": 1}]"#;
        let d = detect_json(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn json_flat_object_has_shallow_depth() {
        let text = r#"{"a": 1, "b": 2}"#;
        let d = detect_json(text);
        assert!(d.confidence > 0.35);
        assert_eq!(d.depth, 1);
    }

    #[test]
    fn invalid_json_scores_zero() {
        let text = "This is not JSON, just a sentence with { random braces } in it.";
        let d = detect_json(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- YAML ---

    #[test]
    fn yaml_key_values_detected() {
        let text = "name: morph\nversion: 0.1.0\nfeatures:\n  - gateway\n  - detect\n";
        let d = detect_yaml(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn yaml_doc_marker_detected() {
        let text = "---\ntitle: Example\nauthor: A. Person\n";
        let d = detect_yaml(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn yaml_nested_mapping_has_depth() {
        let text = "root:\n  child:\n    grandchild: value\n";
        let d = detect_yaml(text);
        assert!(d.confidence > 0.35);
        assert!(d.depth >= 2, "depth was {}", d.depth);
    }

    #[test]
    fn prose_with_stray_colon_scores_zero() {
        let text = "The quick brown fox jumps over the lazy dog in the evening light.";
        let d = detect_yaml(text);
        assert_eq!(d.confidence, 0.0);
    }

    #[test]
    fn tabs_disqualify_yaml() {
        let text = "name:\tmorph\nversion:\t0.1.0\n";
        let d = detect_yaml(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- XML/HTML ---

    #[test]
    fn xml_prolog_detected_as_xml() {
        let text = "<?xml version=\"1.0\"?>\n<root><item>1</item><item>2</item></root>";
        let d = detect_xml(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert!(!d.is_html);
    }

    #[test]
    fn balanced_tags_detected_as_xml() {
        let text =
            "<config><setting name=\"x\">1</setting><setting name=\"y\">2</setting></config>";
        let d = detect_xml(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert!(!d.is_html);
    }

    #[test]
    fn html_tags_detected_as_html() {
        let text = "<!DOCTYPE html>\n<html><head><title>Hi</title></head><body><div>Hello</div></body></html>";
        let d = detect_xml(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert!(d.is_html);
    }

    #[test]
    fn prose_with_angle_brackets_scores_zero() {
        let text = "If a < b and b < c then a < c, which is transitivity in plain math notation.";
        let d = detect_xml(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- CSV ---

    #[test]
    fn simple_csv_detected() {
        let text = "name,age,city\nAlice,30,NYC\nBob,25,LA\nCarol,40,SF\n";
        let d = detect_csv(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.columns, 3);
        assert_eq!(d.rows, 4);
    }

    #[test]
    fn semicolon_csv_detected() {
        let text = "a;b;c\n1;2;3\n4;5;6\n";
        let d = detect_csv(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.columns, 3);
    }

    #[test]
    fn tsv_detected() {
        let text = "a\tb\tc\n1\t2\t3\n4\t5\t6\n";
        let d = detect_csv(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn inconsistent_commas_score_zero() {
        let text = "Hello, world.\nThis, however, has a different, comma count entirely.\nOne more, line.\n";
        let d = detect_csv(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- SQL ---

    #[test]
    fn select_statement_detected() {
        let text = "SELECT id, name FROM users WHERE active = 1 ORDER BY name;";
        let d = detect_sql(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn create_table_detected() {
        let text = "CREATE TABLE users (\n  id INTEGER PRIMARY KEY,\n  name TEXT\n);";
        let d = detect_sql(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn insert_statement_detected() {
        let text = "INSERT INTO users (id, name) VALUES (1, 'Ada');";
        let d = detect_sql(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn prose_mentioning_select_scores_zero() {
        let text = "Please select an option from the list where necessary and update your profile.";
        let d = detect_sql(text);
        assert_eq!(d.confidence, 0.0);
    }
}
