use std::path::Path;
use std::sync::Arc;

use morph_core::content::ContentKind;
use morph_core::error::GatewayError;
use morph_core::traits::{Classifier, Renderer, Transformer};
use morph_plugin_abi::exports::morph::plugin::plugin::PluginInfo;

use crate::adapters::{WasmClassifier, WasmRenderer, WasmTransformer};
use crate::runtime::PluginRuntime;

/// Every WASM plugin found in a directory, already sorted into the trait
/// buckets `morph-gateway` registers alongside native renderers/classifiers/
/// transformers. `infos` is kept separately (rather than requiring callers
/// to downcast trait objects) so `morph plugins` can print name/version/kind
/// without needing to know which bucket a plugin landed in.
#[derive(Default)]
pub struct DiscoveredPlugins {
    pub renderers: Vec<Arc<dyn Renderer>>,
    pub classifiers: Vec<Arc<dyn Classifier>>,
    pub transformers: Vec<Arc<dyn Transformer>>,
    pub infos: Vec<PluginInfo>,
}

fn parse_content_kind(s: &str) -> Option<ContentKind> {
    use ContentKind::*;
    Some(match s {
        "plain_text" => PlainText,
        "markdown" => Markdown,
        "code" => Code,
        "json" => Json,
        "yaml" => Yaml,
        "xml" => Xml,
        "csv" => Csv,
        "sql" => Sql,
        "html" => Html,
        "shell_session" => ShellSession,
        "terminal_log" => TerminalLog,
        "stack_trace" => StackTrace,
        "table" => Table,
        "math" => Math,
        "mermaid" => Mermaid,
        "uml" => Uml,
        "config" => Config,
        "api_spec" => ApiSpec,
        _ => return None,
    })
}

/// Loads every `*.wasm` file directly inside `dir` (non-recursive) and buckets
/// each by its declared `manifest().kind`. A plugin that fails to load or
/// declares an unrecognized kind is logged and skipped — one bad plugin file
/// must never prevent the rest (or the native pipeline) from starting.
pub fn load_plugins_from_dir(
    dir: &Path,
    runtime: &PluginRuntime,
) -> Result<DiscoveredPlugins, GatewayError> {
    let mut discovered = DiscoveredPlugins::default();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(dir = %dir.display(), "plugin directory does not exist, skipping");
            return Ok(discovered);
        }
        Err(e) => {
            return Err(GatewayError::Plugin(format!(
                "failed to read plugin directory {}: {e}",
                dir.display()
            )))
        }
    };

    for entry in entries {
        let entry = entry
            .map_err(|e| GatewayError::Plugin(format!("failed to list plugin directory: {e}")))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }

        let plugin = match runtime.load(&path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping plugin that failed to load");
                continue;
            }
        };
        let plugin = Arc::new(plugin);
        discovered.infos.push(plugin.info.clone());

        match plugin.info.kind.as_str() {
            "renderer" => {
                let supported: Vec<ContentKind> = plugin
                    .info
                    .supported_kinds
                    .iter()
                    .filter_map(|s| {
                        let parsed = parse_content_kind(s);
                        if parsed.is_none() {
                            tracing::warn!(plugin = %plugin.info.name, kind = %s, "unrecognized content kind in supported_kinds, ignoring");
                        }
                        parsed
                    })
                    .collect();
                discovered
                    .renderers
                    .push(Arc::new(WasmRenderer::new(plugin, supported)));
            }
            "classifier" => discovered
                .classifiers
                .push(Arc::new(WasmClassifier::new(plugin))),
            "transformer" => discovered
                .transformers
                .push(Arc::new(WasmTransformer::new(plugin))),
            other => {
                tracing::warn!(plugin = %plugin.info.name, kind = %other, "unrecognized plugin kind, skipping");
            }
        }
    }

    Ok(discovered)
}
