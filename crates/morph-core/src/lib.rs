//! Canonical domain types and stable trait contracts shared by every Morph
//! crate. `morph-core` performs no I/O and depends on nothing beyond
//! `serde`/`serde_json`/`thiserror`/`async-trait`/`futures` — every other
//! crate in the workspace depends on this one, never the other way around.
//!
//! Adding a new provider, protocol, renderer, classifier, or transformer to
//! Morph means implementing one of the traits in [`traits`] against the
//! types defined here; nothing else in the pipeline needs to change.

pub mod capabilities;
pub mod content;
pub mod error;
pub mod message;
pub mod registry;
pub mod representation;
pub mod request;
pub mod stats;
pub mod stream;
pub mod traits;

/// Re-exports of the types most callers need, so downstream crates can
/// `use morph_core::prelude::*;` instead of importing from every submodule.
pub mod prelude {
    pub use crate::capabilities::Capabilities;
    pub use crate::content::{ContentKind, ContentMetrics, DetectedContent, RequestAnalysis};
    pub use crate::error::{GatewayError, Result};
    pub use crate::message::{
        ContentBlock, ImageBlock, ImageSource, Message, Role, TextBlock, ToolChoice,
        ToolDefinition, ToolResultBlock, ToolUseBlock,
    };
    pub use crate::registry::Registry;
    pub use crate::representation::{
        PlannerConfig, PlannerMode, RasterFormat, RenderOptions, RenderedAsset, Representation,
        RepresentationPlan, SegmentPlan, Theme,
    };
    pub use crate::request::{
        CanonicalRequest, CanonicalResponse, ReasoningConfig, ReasoningEffort, RequestMetadata,
        ResponseEvent, ResponseFormat, StopReason, Usage,
    };
    pub use crate::stats::StatsEvent;
    pub use crate::stream::ResponseStream;
    pub use crate::traits::{
        Classifier, Middleware, ProtocolAdapter, ProviderAdapter, Renderer, RepresentationPlanner,
        StatsSink, Transformer,
    };
}
