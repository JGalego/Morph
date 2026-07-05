use serde::{Deserialize, Serialize};

/// The structural type of a piece of content, as determined by `morph-detect`
/// (or a WASM classifier plugin). This drives both renderer selection
/// (`Renderer::supports`) and representation planning heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    PlainText,
    Markdown,
    Code,
    Json,
    Yaml,
    Xml,
    Csv,
    Sql,
    Html,
    ShellSession,
    TerminalLog,
    StackTrace,
    Table,
    Math,
    Mermaid,
    Uml,
    Config,
    ApiSpec,
}

impl ContentKind {
    /// Content kinds where an LLM's own vision-based transcription of an
    /// image would be lossy for anything that must round-trip exactly
    /// (keys, punctuation, indentation). The representation planner uses
    /// this to decide that text must always remain present for these kinds,
    /// with an image only ever added as a supplementary aid.
    pub fn requires_exact_text(&self) -> bool {
        matches!(
            self,
            ContentKind::Code
                | ContentKind::Json
                | ContentKind::Yaml
                | ContentKind::Xml
                | ContentKind::Sql
                | ContentKind::Config
                | ContentKind::ApiSpec
        )
    }
}

/// Lightweight structural signals about a piece of content, computed once by
/// the detector and reused by both the planner and the renderer so neither
/// has to re-scan the raw text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentMetrics {
    pub char_count: usize,
    pub line_count: usize,
    /// Rough token estimate (chars / 4), used for cost/latency heuristics
    /// without pulling in a tokenizer dependency at the core-type level.
    pub estimated_tokens: usize,
    /// Nesting depth for structured formats (JSON/YAML/XML); 0 otherwise.
    pub structural_depth: usize,
    /// Column count for detected tables; 0 otherwise.
    pub column_count: usize,
    /// Row count for detected tables; 0 otherwise.
    pub row_count: usize,
    pub has_ansi_codes: bool,
}

impl ContentMetrics {
    pub fn from_text(text: &str) -> Self {
        ContentMetrics {
            char_count: text.chars().count(),
            line_count: text.lines().count(),
            estimated_tokens: text.chars().count() / 4,
            structural_depth: 0,
            column_count: 0,
            row_count: 0,
            has_ansi_codes: text.contains('\u{1b}'),
        }
    }
}

/// One classified segment of a request: a contiguous piece of text (usually,
/// but not necessarily, a whole message) along with what it was detected as.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedContent {
    pub kind: ContentKind,
    pub raw: String,
    /// Detector confidence in `[0, 1]`. Low-confidence detections fall back
    /// to `ContentKind::PlainText` treatment in the planner.
    pub confidence: f32,
    pub metrics: ContentMetrics,
    /// Best-guess language for `ContentKind::Code` (e.g. "rust", "python").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Index into the originating `CanonicalRequest::messages`, so a
    /// renderer's output can be attached back to the right message. `None`
    /// for content constructed outside of `morph_detect::analyze` (e.g. in
    /// tests, or a future non-message-shaped source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_index: Option<usize>,
}

impl DetectedContent {
    pub fn plain(text: impl Into<String>) -> Self {
        let raw = text.into();
        DetectedContent {
            kind: ContentKind::PlainText,
            metrics: ContentMetrics::from_text(&raw),
            raw,
            confidence: 1.0,
            language: None,
            message_index: None,
        }
    }
}

/// The full set of detected segments across a request, indexed so planning
/// decisions can be traced back to the originating message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestAnalysis {
    pub segments: Vec<DetectedContent>,
}
