//! Example Morph WASM plugin: renders plain text/Markdown content as a
//! styled "sticky note" SVG card. Deliberately simple and deliberately NOT
//! trying to duplicate morph-render's real Markdown/Code/JSON/Table/Log
//! renderers — its only job is to prove the plugin path end-to-end (a
//! third-party-style renderer, built and loaded independently of Morph
//! itself, participating in the same rendering pipeline as the built-ins).
//!
//! Note this crate defines its own small JSON DTOs rather than importing
//! anything from `morph-core`/`morph-plugin-host`: those crates aren't even
//! compiled for this crate's target, and depending on them would defeat the
//! point of the plugin boundary being "any language, any toolchain, just
//! speak the documented JSON shape over the WIT interface".

wit_bindgen::generate!({
    world: "plugin-world",
    path: "../../crates/morph-plugin-abi/wit",
});

use base64::Engine as _;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct RenderInput {
    kind: String,
    raw: String,
    options: RenderOptionsIn,
}

#[derive(Deserialize)]
struct RenderOptionsIn {
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default = "default_max_width")]
    max_width_px: u32,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_max_width() -> u32 {
    1200
}

#[derive(Serialize)]
struct RenderOutput {
    mime: String,
    bytes_base64: String,
    width: u32,
    height: u32,
    alt_text: Option<String>,
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Greedy word-wrap into at most 14 lines of at most `width` characters,
/// preserving existing newlines as paragraph breaks.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.len() + 1 + word.len() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        lines.push(current);
        if lines.len() >= 14 {
            break;
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines.truncate(14);
    lines
}

struct Component;

impl exports::morph::plugin::plugin::Guest for Component {
    fn manifest() -> exports::morph::plugin::plugin::PluginInfo {
        exports::morph::plugin::plugin::PluginInfo {
            name: "example-renderer".to_string(),
            version: "0.1.0".to_string(),
            kind: "renderer".to_string(),
            abi_version: "0.1.0".to_string(),
            supported_kinds: vec!["plain_text".to_string(), "markdown".to_string()],
        }
    }

    fn render(input_json: String) -> Result<String, String> {
        let input: RenderInput =
            serde_json::from_str(&input_json).map_err(|e| format!("invalid render input: {e}"))?;

        // Exists solely so morph-plugin-host's test suite can verify its
        // fuel-based sandboxing actually kills a runaway plugin instead of
        // hanging the host — not a real rendering feature.
        if input.raw == "__morph_test_infinite_loop__" {
            let mut x: u64 = 0;
            loop {
                x = x.wrapping_add(1);
                std::hint::black_box(x);
            }
        }

        let dark = input.options.theme != "light";
        let (bg, fg, border) = if dark {
            ("#1f2430", "#e8e6e3", "#ffcc66")
        } else {
            ("#fff8dc", "#2b2b2b", "#d9a441")
        };

        let width = input.options.max_width_px.clamp(280, 560);
        let text: String = input.raw.chars().take(400).collect();
        let lines = wrap_text(&text, 46);
        let line_height: u32 = 22;
        let padding: u32 = 24;
        let height = padding * 2 + 34 + lines.len() as u32 * line_height;

        let mut body = String::new();
        for (i, line) in lines.iter().enumerate() {
            let y = padding + 34 + (i as u32 + 1) * line_height;
            body.push_str(&format!(
                "  <text x=\"{padding}\" y=\"{y}\" font-family=\"monospace\" font-size=\"14\" fill=\"{fg}\">{}</text>\n",
                escape_xml(line)
            ));
        }

        let fold = width - 22;
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n\
             <rect x=\"1\" y=\"1\" width=\"{w2}\" height=\"{h2}\" rx=\"10\" fill=\"{bg}\" stroke=\"{border}\" stroke-width=\"3\"/>\n\
             <polygon points=\"{fold},1 {width},1 {width},22\" fill=\"{border}\" opacity=\"0.55\"/>\n\
             <text x=\"{padding}\" y=\"{title_y}\" font-family=\"sans-serif\" font-size=\"15\" font-weight=\"bold\" fill=\"{border}\">example-renderer plugin</text>\n\
             {body}\
             </svg>",
            width = width,
            height = height,
            w2 = width - 2,
            h2 = height - 2,
            bg = bg,
            border = border,
            fold = fold,
            padding = padding,
            title_y = padding + 6,
            body = body,
        );

        let bytes_base64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
        let output = RenderOutput {
            mime: "image/svg+xml".to_string(),
            bytes_base64,
            width,
            height,
            alt_text: Some(format!(
                "A sticky-note rendering of {} content, produced by the example WASM plugin.",
                input.kind
            )),
        };
        serde_json::to_string(&output).map_err(|e| format!("failed to encode render output: {e}"))
    }

    fn classify(_text: String) -> Result<String, String> {
        Err("example-renderer only implements render(); it declares kind=\"renderer\"".to_string())
    }

    fn transform_request(_request_json: String) -> Result<String, String> {
        Err("example-renderer only implements render(); it declares kind=\"renderer\"".to_string())
    }

    fn transform_response(_response_json: String) -> Result<String, String> {
        Err("example-renderer only implements render(); it declares kind=\"renderer\"".to_string())
    }
}

export!(Component);
