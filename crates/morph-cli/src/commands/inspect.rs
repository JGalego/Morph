use std::path::PathBuf;
use std::time::SystemTime;

use morph_core::capabilities::Capabilities;
use morph_core::message::Message;
use morph_core::representation::PlannerConfig;
use morph_core::request::{CanonicalRequest, RequestMetadata};
use morph_core::traits::RepresentationPlanner;

pub fn execute(text: Option<String>, file: Option<PathBuf>) -> anyhow::Result<()> {
    let text = match (text, file) {
        (Some(t), _) => t,
        (None, Some(f)) => std::fs::read_to_string(&f)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", f.display()))?,
        (None, None) => {
            anyhow::bail!("provide either TEXT or --file <path>; see `morph inspect --help`")
        }
    };

    let req = CanonicalRequest {
        model: "inspect".to_string(),
        messages: vec![Message::user(text)],
        system: None,
        tools: vec![],
        tool_choice: None,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stream: false,
        stop: vec![],
        response_format: None,
        reasoning: None,
        metadata: RequestMetadata {
            request_id: "inspect".to_string(),
            ingress_protocol: "cli".to_string(),
            received_at: SystemTime::now(),
        },
        extra: serde_json::Value::Null,
    };

    let classifier = morph_detect::DefaultClassifier::new();
    let analysis = morph_detect::analyze(&req, &classifier);
    let planner = morph_detect::DefaultPlanner::new();
    let caps = Capabilities {
        vision: true,
        ..Capabilities::default()
    };
    let cfg = PlannerConfig::default();
    let plan = planner.plan(&analysis, &caps, &cfg);

    if analysis.segments.is_empty() {
        println!("Nothing to analyze (empty input).");
        return Ok(());
    }

    println!("Detected {} segment(s):", analysis.segments.len());
    for (i, seg) in analysis.segments.iter().enumerate() {
        println!(
            "  [{i}] kind={:?} confidence={:.2} chars={} tokens~{}{}",
            seg.kind,
            seg.confidence,
            seg.metrics.char_count,
            seg.metrics.estimated_tokens,
            seg.language
                .as_ref()
                .map(|l| format!(" language={l}"))
                .unwrap_or_default(),
        );
    }
    println!();
    println!("Representation plan (assuming a vision-capable target — real plans also depend on the actual provider's capabilities):");
    for decision in &plan.decisions {
        println!(
            "  segment[{}] -> {:?}   ({})",
            decision.segment_index, decision.representation, decision.reason
        );
    }
    Ok(())
}
