use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use morph_core::capabilities::Capabilities;
use morph_core::message::{ContentBlock, ImageSource, Message};
use morph_core::representation::PlannerConfig;
use morph_core::request::{CanonicalRequest, RequestMetadata};
use morph_core::traits::RepresentationPlanner;

pub async fn execute(
    config_path: &Path,
    text: Option<String>,
    file: Option<PathBuf>,
    save_images: Option<PathBuf>,
) -> anyhow::Result<()> {
    let text = match (text, file) {
        (Some(t), _) => t,
        (None, Some(f)) => std::fs::read_to_string(&f)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", f.display()))?,
        (None, None) => {
            anyhow::bail!("provide either TEXT or --file <path>; see 'morph inspect --help'")
        }
    };

    let mut req = CanonicalRequest {
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

    // Reflects your actual morph.toml's `mode`/`render.*` settings if one
    // exists, so e.g. `mode = "force_image_only"` actually shows up here —
    // falls back to defaults (mode "auto") rather than erroring if there's
    // no config yet, since this is a read-only dry run with no side effects.
    let config = morph_config::load(config_path).ok();
    let cfg = match &config {
        Some(config) => {
            println!(
                "Using config from {} (mode = \"{}\").\n",
                config_path.display(),
                config.mode
            );
            morph_gateway::representation::planner_config_from(config)
        }
        None => {
            println!("No config found at {} — using defaults (mode = \"auto\"). Run 'morph init' first to inspect your actual settings.\n", config_path.display());
            PlannerConfig::default()
        }
    };

    let classifier = morph_detect::DefaultClassifier::new();
    let analysis = morph_detect::analyze(&req, &classifier);
    let planner = morph_detect::DefaultPlanner::new();
    let caps = Capabilities {
        vision: true,
        ..Capabilities::default()
    };
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

    let Some(dir) = save_images else {
        return Ok(());
    };

    // From here on this runs the *exact* function `morph-gateway` calls on
    // every real request (`representation::apply`) against a fully
    // assembled `AppState` (same renderers, same plugins if enabled) —
    // this is not a reimplementation, it's the literal live code path, so
    // whatever gets written to `dir` is byte-for-byte what a real request
    // would attach.
    println!();
    let config = config.unwrap_or_default();
    let (_tx, rx) = tokio::sync::watch::channel(config.clone());
    let state = morph_gateway::build_app_state(&config, rx)?;
    morph_gateway::representation::apply(&state, &mut req, &caps, &cfg).await;

    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dir.display()))?;
    let mut saved = 0usize;
    for (message_index, message) in req.messages.iter().enumerate() {
        for block in &message.content {
            let ContentBlock::Image(image) = block else {
                continue;
            };
            if !image.rendered_by_morph {
                continue;
            }
            let ImageSource::Base64 { data } = &image.source else {
                continue;
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| anyhow::anyhow!("Morph produced invalid base64 image data: {e}"))?;
            let ext = if image.mime.contains("svg") {
                "svg"
            } else {
                "png"
            };
            let path = dir.join(format!("message_{message_index}.{ext}"));
            std::fs::write(&path, &bytes)
                .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
            println!(
                "Wrote {} ({} bytes, mime={})",
                path.display(),
                bytes.len(),
                image.mime
            );
            saved += 1;
        }
    }
    if saved == 0 {
        println!(
            "No images were rendered for this input — either nothing needed one under the current plan, \
             or no renderer/plugin supports the detected content kind."
        );
    }

    Ok(())
}
