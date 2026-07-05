use std::sync::Mutex;

use morph_core::stats::StatsEvent;
use morph_core::traits::StatsSink;
use serde::Serialize;
use std::collections::HashMap;

/// Aggregated outcomes for one `(content_kind, representation)` pair —
/// the raw material a future ML-based `RepresentationPlanner` would train
/// on. Kept as running sums rather than storing every `StatsEvent` so
/// memory use is bounded regardless of traffic volume.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Aggregate {
    pub count: u64,
    pub success_count: u64,
    pub total_latency_ms: u64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_cost_usd: f64,
}

impl Aggregate {
    pub fn avg_latency_ms(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_latency_ms as f64 / self.count as f64
        }
    }

    pub fn success_rate(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.success_count as f64 / self.count as f64
        }
    }
}

fn representation_label(r: &morph_core::representation::Representation) -> &'static str {
    use morph_core::representation::Representation::*;
    match r {
        Text => "text",
        ImageOnly { .. } => "image_only",
        Hybrid { .. } => "hybrid",
    }
}

/// The default `StatsSink`: aggregates every recorded event in memory,
/// keyed by `"{content_kind}:{representation_label}"`. Exposed via `morph
/// inspect`/a metrics endpoint so operators (and, eventually, an ML-based
/// planner) can see which representation is actually winning for each
/// content kind — see the project's adaptive-optimization stretch goal.
#[derive(Default)]
pub struct InMemoryStatsSink {
    aggregates: Mutex<HashMap<String, Aggregate>>,
}

impl InMemoryStatsSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> HashMap<String, Aggregate> {
        self.aggregates.lock().expect("stats lock poisoned").clone()
    }
}

impl StatsSink for InMemoryStatsSink {
    fn record(&self, event: StatsEvent) {
        let key = format!(
            "{:?}:{}",
            event.content_kind,
            representation_label(&event.representation)
        );
        let mut aggregates = self.aggregates.lock().expect("stats lock poisoned");
        let entry = aggregates.entry(key).or_default();
        entry.count += 1;
        if event.success {
            entry.success_count += 1;
        }
        entry.total_latency_ms += event.latency_ms;
        entry.total_tokens_in += event.tokens_in as u64;
        entry.total_tokens_out += event.tokens_out as u64;
        entry.total_cost_usd += event.estimated_cost_usd.unwrap_or(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::content::ContentKind;
    use morph_core::representation::{RasterFormat, Representation};

    #[test]
    fn aggregates_by_kind_and_representation() {
        let sink = InMemoryStatsSink::new();
        sink.record(StatsEvent {
            content_kind: ContentKind::Json,
            representation: Representation::Hybrid {
                format: RasterFormat::Png,
            },
            latency_ms: 100,
            tokens_in: 50,
            tokens_out: 20,
            estimated_cost_usd: Some(0.01),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            success: true,
        });
        sink.record(StatsEvent {
            content_kind: ContentKind::Json,
            representation: Representation::Hybrid {
                format: RasterFormat::Png,
            },
            latency_ms: 200,
            tokens_in: 60,
            tokens_out: 25,
            estimated_cost_usd: Some(0.02),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            success: false,
        });

        let snapshot = sink.snapshot();
        let agg = snapshot
            .get("Json:hybrid")
            .expect("expected an aggregate for Json:hybrid");
        assert_eq!(agg.count, 2);
        assert_eq!(agg.success_count, 1);
        assert_eq!(agg.avg_latency_ms(), 150.0);
        assert_eq!(agg.success_rate(), 0.5);
    }
}
