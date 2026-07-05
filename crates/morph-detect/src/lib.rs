//! Content classification and the default representation planner for Morph.
//!
//! This crate is the seam between "what did the client send us" and "how
//! should we send it upstream": [`DefaultClassifier`] looks at raw text and
//! guesses its structural kind, [`analyze`] turns a whole request into a
//! [`RequestAnalysis`], and [`DefaultPlanner`] turns that analysis plus a
//! target's [`Capabilities`] into a [`RepresentationPlan`] the rendering
//! pipeline (`morph-render`) executes. Nothing here does any rendering or
//! I/O — every function is a pure computation over `&str`/canonical types,
//! which is what makes it safe to unit test exhaustively and to swap out
//! (per the `Classifier`/`RepresentationPlanner` trait seams) for a plugin.

pub mod code;
pub mod logs;
pub mod markdown;
pub mod structured;
pub mod table;

use morph_core::capabilities::Capabilities;
use morph_core::content::{ContentKind, ContentMetrics, DetectedContent, RequestAnalysis};
use morph_core::representation::{
    PlannerConfig, PlannerMode, RasterFormat, Representation, RepresentationPlan, SegmentPlan,
};
use morph_core::request::CanonicalRequest;
use morph_core::traits::{Classifier, RepresentationPlanner};

/// Below this top confidence, the classifier reports "no opinion" (empty
/// vec) rather than a low-quality guess, and callers fall back to
/// `ContentKind::PlainText`. Chosen empirically: our heuristics rarely land
/// between 0.2 and 0.35 for genuine matches, so this catches noise without
/// suppressing real (if imperfect) detections.
const CLASSIFY_THRESHOLD: f32 = 0.35;

/// The built-in [`Classifier`]: runs every detector in this crate over the
/// input text and ranks the results. Stateless — a fresh instance is as
/// good as a shared one.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultClassifier;

impl DefaultClassifier {
    pub fn new() -> Self {
        DefaultClassifier
    }
}

impl Classifier for DefaultClassifier {
    fn name(&self) -> &str {
        "morph-detect/default"
    }

    fn classify(&self, text: &str) -> Vec<(ContentKind, f32)> {
        if text.trim().is_empty() {
            return Vec::new();
        }

        let xml = structured::detect_xml(text);
        let xml_kind = if xml.is_html {
            ContentKind::Html
        } else {
            ContentKind::Xml
        };

        let mut scores: Vec<(ContentKind, f32)> = vec![
            (ContentKind::Markdown, markdown::detect(text).confidence),
            (ContentKind::Code, code::detect(text).confidence),
            (ContentKind::Json, structured::detect_json(text).confidence),
            (ContentKind::Yaml, structured::detect_yaml(text).confidence),
            (xml_kind, xml.confidence),
            (ContentKind::Csv, structured::detect_csv(text).confidence),
            (ContentKind::Sql, structured::detect_sql(text).confidence),
            (
                ContentKind::TerminalLog,
                logs::detect_terminal_log(text).confidence,
            ),
            (
                ContentKind::StackTrace,
                logs::detect_stack_trace(text).confidence,
            ),
            (
                ContentKind::ShellSession,
                logs::detect_shell_session(text).confidence,
            ),
            (ContentKind::Table, table::detect(text).confidence),
        ];

        scores.retain(|(_, confidence)| *confidence > 0.0);
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        match scores.first() {
            Some((_, top)) if *top >= CLASSIFY_THRESHOLD => scores,
            _ => Vec::new(),
        }
    }
}

/// Runs `classifier` over every non-empty message in `req` and assembles a
/// [`RequestAnalysis`] in message order. Segments for empty/whitespace-only
/// messages are skipped entirely (there's nothing to classify or render).
///
/// Kind-specific extra metrics (`structural_depth`/`column_count`/
/// `row_count`/`language`) aren't part of the `Classifier` trait's return
/// value (that trait is intentionally minimal so third-party/WASM
/// classifiers only need to return `(kind, confidence)` pairs), so for the
/// kinds this crate itself knows how to detect, `analyze` re-derives those
/// extras from the matching detector module directly.
pub fn analyze(req: &CanonicalRequest, classifier: &dyn Classifier) -> RequestAnalysis {
    let mut segments = Vec::with_capacity(req.messages.len());

    for (message_index, message) in req.messages.iter().enumerate() {
        let text = message.text_content();
        if text.trim().is_empty() {
            continue;
        }

        let candidates = classifier.classify(&text);
        let (kind, confidence) = candidates
            .first()
            .copied()
            .unwrap_or((ContentKind::PlainText, 1.0));

        let mut metrics = ContentMetrics::from_text(&text);
        let mut language = None;

        match kind {
            ContentKind::Code => {
                language = code::detect(&text).language;
            }
            ContentKind::Json => {
                metrics.structural_depth = structured::detect_json(&text).depth;
            }
            ContentKind::Yaml => {
                metrics.structural_depth = structured::detect_yaml(&text).depth;
            }
            ContentKind::Csv => {
                let csv = structured::detect_csv(&text);
                metrics.column_count = csv.columns;
                metrics.row_count = csv.rows;
            }
            ContentKind::Table => {
                let t = table::detect(&text);
                metrics.column_count = t.columns;
                metrics.row_count = t.rows;
            }
            _ => {}
        }

        segments.push(DetectedContent {
            kind,
            raw: text,
            confidence,
            metrics,
            language,
            message_index: Some(message_index),
        });
    }

    RequestAnalysis { segments }
}

/// The built-in [`RepresentationPlanner`]: a fixed set of heuristic rules
/// (documented on [`decide`]) that never changes behavior based on anything
/// but the segment's detected kind/metrics, the target's capabilities, and
/// the operator's [`PlannerConfig`]. Stateless, like [`DefaultClassifier`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPlanner;

impl DefaultPlanner {
    pub fn new() -> Self {
        DefaultPlanner
    }
}

impl RepresentationPlanner for DefaultPlanner {
    fn plan(
        &self,
        analysis: &RequestAnalysis,
        caps: &Capabilities,
        cfg: &PlannerConfig,
    ) -> RepresentationPlan {
        let decisions = analysis
            .segments
            .iter()
            .enumerate()
            .map(|(segment_index, segment)| {
                let (representation, reason) = decide(segment, caps, cfg);
                SegmentPlan {
                    segment_index,
                    representation,
                    reason,
                }
            })
            .collect();

        RepresentationPlan { decisions }
    }
}

/// Applies the planner's heuristic rules, in order, to one segment. Each
/// early return corresponds to one lettered rule from the crate's design
/// doc, and the `reason` string names it so a decision can be traced back
/// via `morph inspect`.
fn decide(
    segment: &DetectedContent,
    caps: &Capabilities,
    cfg: &PlannerConfig,
) -> (Representation, String) {
    // Rule a: an operator override always wins outright.
    if cfg.mode == PlannerMode::ForceText {
        return (Representation::Text, "forced by config".to_string());
    }

    // Rule b: no point planning an image the target literally cannot see.
    if !caps.vision {
        return (
            Representation::Text,
            "target has no vision capability".to_string(),
        );
    }

    let force_hybrid = cfg.mode == PlannerMode::ForceHybrid;
    let format = RasterFormat::Png;

    // Rule c: below a minimum size, rendering overhead never pays for
    // itself. Rule g carves out an explicit exception: ForceHybrid mode
    // skips *only* this cutoff (and the size cutoffs in rules d/e below),
    // while still respecting the exact-text/allow_code_as_image gating.
    if !force_hybrid && segment.metrics.estimated_tokens < cfg.min_tokens_for_rendering {
        return (
            Representation::Text,
            "below rendering threshold".to_string(),
        );
    }

    match segment.kind {
        // Rule d (Code): text always stays; an image is only ever additive,
        // and only when the operator has explicitly opted in, since coding
        // agents need exact, editable text.
        ContentKind::Code => {
            if cfg.allow_code_as_image {
                (
                    Representation::Hybrid { format },
                    "code kind with allow_code_as_image enabled (rule d)".to_string(),
                )
            } else {
                (
                    Representation::Text,
                    "code rendering as image disabled by config (rule d)".to_string(),
                )
            }
        }

        // Rule d (the other requires_exact_text() kinds): text always
        // stays; a Hybrid image is added when the content is large/nested
        // enough that a picture plausibly helps. ApiSpec is excluded here
        // even though `ContentKind::requires_exact_text()` covers it,
        // because rule f explicitly calls it out as always-Text (no
        // detector in this crate ever produces it, and image rendering for
        // API specs is out of scope for v1 regardless).
        ContentKind::Json
        | ContentKind::Yaml
        | ContentKind::Xml
        | ContentKind::Sql
        | ContentKind::Config => {
            let naturally_large =
                segment.metrics.structural_depth > 3 || segment.metrics.line_count > 40;
            if naturally_large {
                (
                    Representation::Hybrid { format },
                    "structured content large/nested enough for a supplementary image (rule d)"
                        .to_string(),
                )
            } else if force_hybrid {
                (
                    Representation::Hybrid { format },
                    "forced hybrid by config; below the normal size threshold (rule g)".to_string(),
                )
            } else {
                (
                    Representation::Text,
                    "structured content too small to warrant a supplementary image (rule d)"
                        .to_string(),
                )
            }
        }

        // Rule e: tables and console-ish output get a supplementary image
        // once they're big enough (or, for logs, colorized) that a picture
        // is more scannable than a wall of text.
        ContentKind::Table => {
            let naturally_large =
                segment.metrics.column_count > 6 || segment.metrics.row_count > 15;
            if naturally_large {
                (
                    Representation::Hybrid { format },
                    "table large enough to benefit from a rendered image (rule e)".to_string(),
                )
            } else if force_hybrid {
                (
                    Representation::Hybrid { format },
                    "forced hybrid by config; table below the normal size threshold (rule g)"
                        .to_string(),
                )
            } else {
                (
                    Representation::Text,
                    "table too small to benefit from a rendered image (rule e)".to_string(),
                )
            }
        }
        ContentKind::TerminalLog | ContentKind::StackTrace | ContentKind::ShellSession => {
            let naturally_large = segment.metrics.has_ansi_codes || segment.metrics.line_count > 30;
            if naturally_large {
                (
                    Representation::Hybrid { format },
                    "log/trace content is ANSI-colored or long enough to benefit from an image (rule e)".to_string(),
                )
            } else if force_hybrid {
                (
                    Representation::Hybrid { format },
                    "forced hybrid by config; log/trace below the normal size threshold (rule g)"
                        .to_string(),
                )
            } else {
                (
                    Representation::Text,
                    "log/trace content too small to benefit from a rendered image (rule e)"
                        .to_string(),
                )
            }
        }

        // Rule f: prose and anything v1 has no image treatment for at all
        // (diagrams, math, API specs, plain CSV, and any future kind not
        // explicitly handled above) always stays text — including under
        // ForceHybrid, since rule g only ever prefers Hybrid "wherever the
        // content kind allows an image at all", and these kinds don't.
        _ => (
            Representation::Text,
            "content kind has no image representation in v1 (rule f)".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::message::Message;
    use morph_core::request::RequestMetadata;
    use std::time::SystemTime;

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            request_id: "req-1".to_string(),
            ingress_protocol: "test".to_string(),
            received_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn request(messages: Vec<Message>) -> CanonicalRequest {
        CanonicalRequest {
            model: "test-model".to_string(),
            messages,
            system: None,
            tools: Vec::new(),
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: false,
            stop: Vec::new(),
            response_format: None,
            reasoning: None,
            metadata: metadata(),
            extra: serde_json::Value::Null,
        }
    }

    fn segment(kind: ContentKind, metrics: ContentMetrics) -> DetectedContent {
        DetectedContent {
            kind,
            raw: "irrelevant".to_string(),
            confidence: 0.9,
            metrics,
            language: None,
            message_index: None,
        }
    }

    fn vision_caps() -> Capabilities {
        Capabilities {
            vision: true,
            ..Capabilities::default()
        }
    }

    fn big_metrics() -> ContentMetrics {
        ContentMetrics {
            estimated_tokens: 1000,
            line_count: 100,
            ..ContentMetrics::default()
        }
    }

    // --- DefaultClassifier ---

    #[test]
    fn classifier_ranks_json_over_noise() {
        let classifier = DefaultClassifier::new();
        let results = classifier.classify(r#"{"a": {"b": {"c": 1}}}"#);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, ContentKind::Json);
    }

    #[test]
    fn classifier_returns_empty_for_ambiguous_short_text() {
        let classifier = DefaultClassifier::new();
        let results = classifier.classify("ok");
        assert!(results.is_empty());
    }

    #[test]
    fn classifier_returns_empty_for_empty_text() {
        let classifier = DefaultClassifier::new();
        assert!(classifier.classify("").is_empty());
        assert!(classifier.classify("   \n  ").is_empty());
    }

    // --- analyze() ---

    #[test]
    fn analyze_segments_multi_message_request_in_order() {
        let classifier = DefaultClassifier::new();
        let req = request(vec![
            Message::user("# Title\n\n- one\n- two\n- three\n\n```rust\nfn f() {}\n```\n"),
            Message::assistant(r#"{"nested": {"deep": {"value": 1}}}"#),
            Message::user("   "), // whitespace-only, should be skipped
            Message::user("SELECT * FROM users WHERE id = 1;"),
        ]);

        let analysis = analyze(&req, &classifier);

        assert_eq!(analysis.segments.len(), 3);
        assert_eq!(analysis.segments[0].kind, ContentKind::Markdown);
        assert_eq!(analysis.segments[1].kind, ContentKind::Json);
        assert!(analysis.segments[1].metrics.structural_depth >= 2);
        assert_eq!(analysis.segments[2].kind, ContentKind::Sql);
    }

    #[test]
    fn analyze_fills_language_for_code_segments() {
        let classifier = DefaultClassifier::new();
        let req = request(vec![Message::user(
            "pub fn add(a: i32, b: i32) -> i32 {\n    let mut sum = a;\n    sum += b;\n    println!(\"{}\", sum);\n    sum\n}\n",
        )]);

        let analysis = analyze(&req, &classifier);

        assert_eq!(analysis.segments.len(), 1);
        assert_eq!(analysis.segments[0].kind, ContentKind::Code);
        assert_eq!(analysis.segments[0].language, Some("rust".to_string()));
    }

    #[test]
    fn analyze_falls_back_to_plain_text_for_low_confidence() {
        let classifier = DefaultClassifier::new();
        let req = request(vec![Message::user("just a short ambiguous reply")]);

        let analysis = analyze(&req, &classifier);

        assert_eq!(analysis.segments.len(), 1);
        assert_eq!(analysis.segments[0].kind, ContentKind::PlainText);
    }

    // --- DefaultPlanner ---

    #[test]
    fn rule_a_force_text_overrides_everything() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, big_metrics())],
        };
        let cfg = PlannerConfig {
            mode: PlannerMode::ForceText,
            ..PlannerConfig::default()
        };

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
        assert_eq!(plan.decisions[0].reason, "forced by config");
    }

    #[test]
    fn rule_b_no_vision_forces_text() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, big_metrics())],
        };
        let caps = Capabilities {
            vision: false,
            ..Capabilities::default()
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &caps, &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
        assert_eq!(plan.decisions[0].reason, "target has no vision capability");
    }

    #[test]
    fn rule_c_below_token_threshold_stays_text() {
        let planner = DefaultPlanner::new();
        let tiny = ContentMetrics {
            estimated_tokens: 5,
            line_count: 50,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, tiny)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
        assert_eq!(plan.decisions[0].reason, "below rendering threshold");
    }

    #[test]
    fn rule_d_code_stays_text_by_default() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Code, big_metrics())],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn rule_d_code_becomes_hybrid_when_opted_in() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Code, big_metrics())],
        };
        let cfg = PlannerConfig {
            allow_code_as_image: true,
            ..PlannerConfig::default()
        };

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(
            plan.decisions[0].representation,
            Representation::Hybrid {
                format: RasterFormat::Png
            }
        );
    }

    #[test]
    fn rule_d_json_hybrid_when_deeply_nested() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 1000,
            structural_depth: 5,
            line_count: 10,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, metrics)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(
            plan.decisions[0].representation,
            Representation::Hybrid {
                format: RasterFormat::Png
            }
        );
    }

    #[test]
    fn rule_d_json_stays_text_when_small() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 1000,
            structural_depth: 1,
            line_count: 5,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, metrics)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn rule_e_large_table_becomes_hybrid() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 1000,
            column_count: 8,
            row_count: 20,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Table, metrics)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(
            plan.decisions[0].representation,
            Representation::Hybrid {
                format: RasterFormat::Png
            }
        );
    }

    #[test]
    fn rule_e_small_table_stays_text() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 1000,
            column_count: 3,
            row_count: 4,
            line_count: 5,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Table, metrics)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn rule_e_ansi_log_becomes_hybrid_even_if_short() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 1000,
            line_count: 5,
            has_ansi_codes: true,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::TerminalLog, metrics)],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(
            plan.decisions[0].representation,
            Representation::Hybrid {
                format: RasterFormat::Png
            }
        );
    }

    #[test]
    fn rule_f_markdown_never_gets_an_image() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Markdown, big_metrics())],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn rule_g_force_hybrid_bypasses_size_cutoff() {
        let planner = DefaultPlanner::new();
        // Small enough that rule c/d would normally keep this as Text.
        let metrics = ContentMetrics {
            estimated_tokens: 5,
            structural_depth: 1,
            line_count: 3,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Json, metrics)],
        };
        let cfg = PlannerConfig {
            mode: PlannerMode::ForceHybrid,
            ..PlannerConfig::default()
        };

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(
            plan.decisions[0].representation,
            Representation::Hybrid {
                format: RasterFormat::Png
            }
        );
    }

    #[test]
    fn rule_g_force_hybrid_still_respects_code_gating() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 5,
            line_count: 3,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Code, metrics)],
        };
        let cfg = PlannerConfig {
            mode: PlannerMode::ForceHybrid,
            ..PlannerConfig::default()
        };

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        // allow_code_as_image is still false, so ForceHybrid must not
        // override it.
        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn rule_g_force_hybrid_never_touches_prose() {
        let planner = DefaultPlanner::new();
        let metrics = ContentMetrics {
            estimated_tokens: 5,
            line_count: 3,
            ..ContentMetrics::default()
        };
        let analysis = RequestAnalysis {
            segments: vec![segment(ContentKind::Markdown, metrics)],
        };
        let cfg = PlannerConfig {
            mode: PlannerMode::ForceHybrid,
            ..PlannerConfig::default()
        };

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions[0].representation, Representation::Text);
    }

    #[test]
    fn plan_preserves_segment_order_and_indices() {
        let planner = DefaultPlanner::new();
        let analysis = RequestAnalysis {
            segments: vec![
                segment(ContentKind::Markdown, big_metrics()),
                segment(ContentKind::Json, big_metrics()),
            ],
        };
        let cfg = PlannerConfig::default();

        let plan = planner.plan(&analysis, &vision_caps(), &cfg);

        assert_eq!(plan.decisions.len(), 2);
        assert_eq!(plan.decisions[0].segment_index, 0);
        assert_eq!(plan.decisions[1].segment_index, 1);
    }
}
