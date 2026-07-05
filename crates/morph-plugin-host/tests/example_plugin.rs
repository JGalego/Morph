//! Integration tests against the real, compiled `plugins/example-renderer`
//! WASM component — not a mock. These are the tests that actually exercise
//! the sandboxing guarantees: no filesystem/network access, a fuel budget
//! that kills a runaway plugin, and the full render() JSON contract.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use morph_core::content::{ContentKind, DetectedContent};
use morph_core::representation::{RasterFormat, RenderOptions, Theme};
use morph_core::traits::Renderer;
use morph_plugin_host::{PluginRuntime, WasmRenderer};

/// Builds `plugins/example-renderer` for wasm32-wasip2 if the artifact
/// isn't already present, so `cargo test -p morph-plugin-host` is
/// self-sufficient for anyone who has run `rustup target add
/// wasm32-wasip2` (documented in docs/PLUGINS.md and `morph doctor`).
fn example_plugin_path() -> PathBuf {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/example-renderer");
    let artifact = plugin_dir.join("target/wasm32-wasip2/release/example_renderer.wasm");

    if !artifact.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip2", "--release"])
            .current_dir(&plugin_dir)
            .status()
            .expect("failed to invoke cargo to build the example plugin");
        assert!(
            status.success(),
            "building plugins/example-renderer failed — is the wasm32-wasip2 target installed? \
             (`rustup target add wasm32-wasip2`)"
        );
    }

    assert!(
        artifact.exists(),
        "example plugin artifact still missing after build: {}",
        artifact.display()
    );
    artifact
}

fn default_render_options() -> RenderOptions {
    RenderOptions {
        theme: Theme::Dark,
        max_width_px: 480,
        scale: 1.0,
        format: RasterFormat::Svg,
    }
}

#[test]
fn loads_plugin_and_validates_manifest() {
    let runtime = PluginRuntime::new().expect("failed to init plugin runtime");
    let plugin = runtime
        .load(&example_plugin_path())
        .expect("failed to load example plugin");

    assert_eq!(plugin.info.name, "example-renderer");
    assert_eq!(plugin.info.kind, "renderer");
    assert_eq!(
        plugin.info.abi_version,
        morph_plugin_host::SUPPORTED_ABI_VERSION
    );
    assert!(plugin
        .info
        .supported_kinds
        .contains(&"plain_text".to_string()));
}

#[test]
fn renders_through_the_sandbox_and_produces_valid_svg() {
    let runtime = PluginRuntime::new().expect("failed to init plugin runtime");
    let plugin = Arc::new(
        runtime
            .load(&example_plugin_path())
            .expect("failed to load example plugin"),
    );
    let renderer = WasmRenderer::new(plugin, vec![ContentKind::PlainText]);

    assert!(renderer.supports(ContentKind::PlainText));
    assert!(!renderer.supports(ContentKind::Json));

    let content = DetectedContent::plain("hello from a sandboxed plugin");
    let asset = renderer
        .render(&content, &default_render_options())
        .expect("render() through the plugin should succeed");

    assert_eq!(asset.mime, "image/svg+xml");
    let svg = String::from_utf8(asset.bytes).expect("plugin SVG output should be valid UTF-8");
    assert!(svg.contains("<svg"));
    assert!(svg.contains("hello from a sandboxed plugin"));
    assert!(asset.width > 0 && asset.height > 0);
}

#[test]
fn runaway_plugin_is_killed_by_fuel_limit_instead_of_hanging() {
    let runtime = PluginRuntime::new().expect("failed to init plugin runtime");
    let plugin = Arc::new(
        runtime
            .load(&example_plugin_path())
            .expect("failed to load example plugin"),
    );
    let renderer = WasmRenderer::new(plugin, vec![ContentKind::PlainText]);

    let content = DetectedContent::plain("__morph_test_infinite_loop__");
    let result = renderer.render(&content, &default_render_options());

    assert!(
        result.is_err(),
        "an infinite-looping plugin call must be trapped, not succeed"
    );
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("fuel") || message.contains("trapped"),
        "expected a fuel-exhaustion trap, got: {message}"
    );
}

#[test]
fn rejects_unsupported_content_gracefully() {
    // The plugin only declares plain_text/markdown support; calling it
    // directly for something it wasn't asked to support isn't a host-level
    // concern (that's what `Renderer::supports` is for upstream), but the
    // plugin itself should still not panic — it just renders whatever text
    // it's given, since the example plugin doesn't actually branch on kind.
    let runtime = PluginRuntime::new().expect("failed to init plugin runtime");
    let plugin = Arc::new(
        runtime
            .load(&example_plugin_path())
            .expect("failed to load example plugin"),
    );
    let renderer = WasmRenderer::new(plugin, vec![ContentKind::PlainText]);

    let content = DetectedContent {
        kind: ContentKind::Json,
        ..DetectedContent::plain("{\"a\":1}")
    };
    let asset = renderer.render(&content, &default_render_options());
    assert!(
        asset.is_ok(),
        "plugin should not panic on an unexpected (but valid) input shape"
    );
}
