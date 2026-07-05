use serde::{Deserialize, Serialize};

use crate::content::ContentKind;
use crate::representation::Representation;

/// One recorded outcome, emitted after each request completes. This is the
/// raw material for adaptive optimization: aggregating these by
/// `(content_kind, representation)` is what lets a future planner learn
/// which representation actually performs best, instead of relying on the
/// static heuristics baked into the default `RepresentationPlanner`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsEvent {
    pub content_kind: ContentKind,
    pub representation: Representation,
    pub latency_ms: u64,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub estimated_cost_usd: Option<f64>,
    pub provider: String,
    pub model: String,
    pub success: bool,
}
