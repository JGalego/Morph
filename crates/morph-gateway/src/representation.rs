use base64::Engine as _;
use morph_config::Config;
use morph_core::capabilities::Capabilities;
use morph_core::message::{ContentBlock, ImageBlock, ImageSource};
use morph_core::representation::{
    PlannerConfig, PlannerMode, RenderOptions, Representation, Theme,
};
use morph_core::request::CanonicalRequest;
use morph_core::stats::StatsEvent;
use morph_core::traits::StatsSink;

use crate::state::AppState;

fn parse_theme(s: &str) -> Theme {
    if s.eq_ignore_ascii_case("light") {
        Theme::Light
    } else {
        Theme::Dark
    }
}

fn parse_mode(s: &str) -> PlannerMode {
    match s {
        "force_text" => PlannerMode::ForceText,
        "force_hybrid" => PlannerMode::ForceHybrid,
        _ => PlannerMode::Auto,
    }
}

pub fn planner_config_from(config: &Config) -> PlannerConfig {
    PlannerConfig {
        mode: parse_mode(&config.mode),
        theme: parse_theme(&config.theme),
        min_tokens_for_rendering: config.render.min_tokens_for_rendering,
        allow_code_as_image: config.render.allow_code_as_image,
    }
}

/// Runs detect → plan → render and attaches any rendered images directly
/// onto the originating messages in `req`, mutating it in place. Errors
/// from an individual renderer are logged and skipped rather than failing
/// the request — a rendering failure must never turn into a lost prompt;
/// worst case, the segment simply stays text-only.
pub async fn apply(
    state: &AppState,
    req: &mut CanonicalRequest,
    caps: &Capabilities,
    planner_cfg: &PlannerConfig,
) {
    let analysis = morph_detect::analyze(req, state.classifier.as_ref());
    if analysis.segments.is_empty() {
        return;
    }

    let plan = state.planner.plan(&analysis, caps, planner_cfg);

    for decision in &plan.decisions {
        let format = match &decision.representation {
            Representation::Text => continue,
            Representation::Hybrid { format } | Representation::ImageOnly { format } => *format,
        };
        let Some(segment) = analysis.segments.get(decision.segment_index) else {
            continue;
        };
        let Some(message_index) = segment.message_index else {
            continue;
        };
        let Some(renderer) = state
            .renderers
            .iter()
            .find(|r| r.supports(segment.kind))
            .cloned()
        else {
            tracing::debug!(kind = ?segment.kind, reason = %decision.reason, "no renderer registered for this content kind, leaving as text");
            continue;
        };

        let render_opts = RenderOptions {
            theme: planner_cfg.theme,
            max_width_px: 1200,
            scale: 1.0,
            format,
        };
        let content = segment.clone();
        let start = std::time::Instant::now();
        let render_result =
            tokio::task::spawn_blocking(move || renderer.render(&content, &render_opts)).await;

        let success = match render_result {
            Ok(Ok(asset)) => {
                if let Some(message) = req.messages.get_mut(message_index) {
                    let image_block = ContentBlock::Image(ImageBlock {
                        mime: asset.mime,
                        source: ImageSource::Base64 {
                            data: base64::engine::general_purpose::STANDARD.encode(&asset.bytes),
                        },
                        rendered_by_morph: true,
                    });
                    if matches!(decision.representation, Representation::ImageOnly { .. }) {
                        message.content = vec![image_block];
                    } else {
                        message.content.push(image_block);
                    }
                }
                true
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, kind = ?segment.kind, "renderer failed, leaving segment as text");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "renderer task panicked, leaving segment as text");
                false
            }
        };

        state.stats_sink.record(StatsEvent {
            content_kind: segment.kind,
            representation: decision.representation.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
            tokens_in: (segment.metrics.estimated_tokens) as u32,
            tokens_out: 0,
            estimated_cost_usd: None,
            provider: String::new(),
            model: req.model.clone(),
            success,
        });
    }
}

/// Whether `req` contains anything the planner could act on at all — used
/// to skip the detect/plan pass entirely for trivial requests.
pub fn worth_analyzing(req: &CanonicalRequest) -> bool {
    req.messages
        .iter()
        .any(|m| !m.text_content().trim().is_empty())
}
