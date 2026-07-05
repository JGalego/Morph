use std::sync::Arc;

use base64::Engine as _;
use morph_core::content::{ContentKind, DetectedContent};
use morph_core::error::GatewayError;
use morph_core::representation::{RenderOptions, RenderedAsset};
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use morph_core::traits::{Classifier, Renderer, Transformer};
use serde::{Deserialize, Serialize};

use crate::runtime::LoadedPlugin;

/// Wire shape for the renderer entry point. `morph_core::representation::
/// RenderOptions`/`RenderedAsset` deliberately don't derive `Serialize`
/// (nothing else in the native pipeline needs to serialize them), and
/// `RenderedAsset::bytes` needs base64 framing to survive a WIT `string`, so
/// this crate defines its own small DTOs rather than adding serde derives to
/// core types purely for the plugin boundary's sake.
#[derive(Serialize)]
struct RenderInputWire<'a> {
    kind: ContentKind,
    raw: &'a str,
    metrics: &'a morph_core::content::ContentMetrics,
    language: &'a Option<String>,
    options: RenderOptionsWire,
}

#[derive(Serialize)]
struct RenderOptionsWire {
    theme: morph_core::representation::Theme,
    max_width_px: u32,
    scale: f32,
    format: morph_core::representation::RasterFormat,
}

#[derive(Deserialize)]
struct RenderOutputWire {
    mime: String,
    bytes_base64: String,
    width: u32,
    height: u32,
    #[serde(default)]
    alt_text: Option<String>,
}

/// Adapts a loaded WASM plugin to `morph_core::traits::Renderer`. From the
/// rest of the gateway's point of view this is indistinguishable from a
/// native renderer — the same trait, called the same way — the only
/// difference is the call is routed through a sandboxed instance.
pub struct WasmRenderer {
    plugin: Arc<LoadedPlugin>,
    supported: Vec<ContentKind>,
}

impl WasmRenderer {
    pub fn new(plugin: Arc<LoadedPlugin>, supported: Vec<ContentKind>) -> Self {
        WasmRenderer { plugin, supported }
    }
}

impl Renderer for WasmRenderer {
    fn name(&self) -> &str {
        &self.plugin.info.name
    }

    fn supports(&self, kind: ContentKind) -> bool {
        self.supported.contains(&kind)
    }

    fn render(
        &self,
        content: &DetectedContent,
        opts: &RenderOptions,
    ) -> Result<RenderedAsset, GatewayError> {
        let input = RenderInputWire {
            kind: content.kind,
            raw: &content.raw,
            metrics: &content.metrics,
            language: &content.language,
            options: RenderOptionsWire {
                theme: opts.theme,
                max_width_px: opts.max_width_px,
                scale: opts.scale,
                format: opts.format,
            },
        };
        let input_json = serde_json::to_string(&input)
            .map_err(|e| GatewayError::Plugin(format!("failed to encode renderer input: {e}")))?;

        let output_json = self.plugin.call_render(&input_json)?;
        let output: RenderOutputWire = serde_json::from_str(&output_json).map_err(|e| {
            GatewayError::Plugin(format!(
                "plugin {} returned malformed render output: {e}",
                self.plugin.info.name
            ))
        })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(output.bytes_base64)
            .map_err(|e| {
                GatewayError::Plugin(format!(
                    "plugin {} returned invalid base64 image bytes: {e}",
                    self.plugin.info.name
                ))
            })?;

        Ok(RenderedAsset {
            mime: output.mime,
            bytes,
            width: output.width,
            height: output.height,
            alt_text: output.alt_text,
        })
    }
}

/// Adapts a loaded WASM plugin to `morph_core::traits::Classifier`.
pub struct WasmClassifier {
    plugin: Arc<LoadedPlugin>,
}

impl WasmClassifier {
    pub fn new(plugin: Arc<LoadedPlugin>) -> Self {
        WasmClassifier { plugin }
    }
}

impl Classifier for WasmClassifier {
    fn name(&self) -> &str {
        &self.plugin.info.name
    }

    fn classify(&self, text: &str) -> Vec<(ContentKind, f32)> {
        let result_json = match self.plugin.call_classify(text) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!(plugin = %self.plugin.info.name, error = %e, "classifier plugin call failed");
                return Vec::new();
            }
        };
        match serde_json::from_str::<Vec<(ContentKind, f32)>>(&result_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(plugin = %self.plugin.info.name, error = %e, "classifier plugin returned malformed output");
                Vec::new()
            }
        }
    }
}

/// Adapts a loaded WASM plugin to `morph_core::traits::Transformer`.
pub struct WasmTransformer {
    plugin: Arc<LoadedPlugin>,
}

impl WasmTransformer {
    pub fn new(plugin: Arc<LoadedPlugin>) -> Self {
        WasmTransformer { plugin }
    }
}

impl Transformer for WasmTransformer {
    fn name(&self) -> &str {
        &self.plugin.info.name
    }

    fn transform_request(&self, req: CanonicalRequest) -> Result<CanonicalRequest, GatewayError> {
        let input_json = serde_json::to_string(&req).map_err(|e| {
            GatewayError::Plugin(format!("failed to encode request for plugin: {e}"))
        })?;
        let output_json = self.plugin.call_transform_request(&input_json)?;
        serde_json::from_str(&output_json).map_err(|e| {
            GatewayError::Plugin(format!(
                "plugin {} returned a malformed transformed request: {e}",
                self.plugin.info.name
            ))
        })
    }

    fn transform_response(
        &self,
        resp: CanonicalResponse,
    ) -> Result<CanonicalResponse, GatewayError> {
        let input_json = serde_json::to_string(&resp).map_err(|e| {
            GatewayError::Plugin(format!("failed to encode response for plugin: {e}"))
        })?;
        let output_json = self.plugin.call_transform_response(&input_json)?;
        serde_json::from_str(&output_json).map_err(|e| {
            GatewayError::Plugin(format!(
                "plugin {} returned a malformed transformed response: {e}",
                self.plugin.info.name
            ))
        })
    }
}
