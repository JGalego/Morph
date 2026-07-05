use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RasterFormat {
    Svg,
    Png,
    Jpeg,
    WebP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
}

/// What a segment of content should become before it's sent to the provider.
///
/// `Hybrid` is intentionally the common case for anything where
/// `ContentKind::requires_exact_text()` is true: the raw text is always kept
/// (for exact fidelity) and the image is additive, never a replacement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Representation {
    /// Leave the segment as plain text, unmodified.
    Text,
    /// Replace the segment with a rendered image only (no text kept).
    /// Only ever chosen for content kinds where `requires_exact_text()` is false.
    ImageOnly { format: RasterFormat },
    /// Keep the original text AND add a rendered image alongside it.
    Hybrid { format: RasterFormat },
}

impl Representation {
    pub fn is_text_only(&self) -> bool {
        matches!(self, Representation::Text)
    }

    pub fn wants_image(&self) -> bool {
        !matches!(self, Representation::Text)
    }
}

/// The plan for one segment, carrying a human-readable `reason` so planning
/// decisions are debuggable via `morph inspect` and observability exports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub segment_index: usize,
    pub representation: Representation,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepresentationPlan {
    pub decisions: Vec<SegmentPlan>,
}

/// User/operator-configurable knobs the planner consults before applying its
/// heuristics. Lives in `morph-core` (not `morph-config`) because the
/// `RepresentationPlanner` trait takes it by reference and core must not
/// depend on the config crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerConfig {
    /// "auto" heuristics, or force everything to one representation.
    pub mode: PlannerMode,
    pub theme: Theme,
    /// Below this estimated-token threshold, rendering overhead is never
    /// worth it regardless of content kind.
    pub min_tokens_for_rendering: usize,
    /// Render code as an image is off by default: coding agents need exact,
    /// editable text, and Morph must never silently degrade that.
    pub allow_code_as_image: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        PlannerConfig {
            mode: PlannerMode::Auto,
            theme: Theme::Dark,
            min_tokens_for_rendering: 120,
            allow_code_as_image: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerMode {
    Auto,
    ForceText,
    ForceHybrid,
    /// Replace text with a rendered image wherever that's safe — prose,
    /// Markdown, tables, logs, stack traces. Content kinds where an LLM's
    /// own vision transcription would be lossy for something that must
    /// round-trip exactly (`ContentKind::requires_exact_text()`: JSON/YAML/
    /// XML/SQL/config/code) are a deliberate exception: this mode still
    /// only ever *adds* an image alongside that text, matching
    /// `ForceHybrid` for those kinds specifically, and never drops it.
    ForceImageOnly,
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub theme: Theme,
    pub max_width_px: u32,
    /// Scale factor applied on top of a 96-DPI baseline (2.0 = retina-ish).
    pub scale: f32,
    pub format: RasterFormat,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            theme: Theme::Dark,
            max_width_px: 1200,
            scale: 1.0,
            format: RasterFormat::Png,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedAsset {
    pub mime: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Accessibility / model-hint text describing the rendered image,
    /// carried alongside it when representation is `Hybrid`.
    pub alt_text: Option<String>,
}
