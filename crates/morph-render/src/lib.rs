//! Morph's rendering engine: turns a [`DetectedContent`] segment into a
//! [`RenderedAsset`] (SVG or PNG) that a `RepresentationPlan` can hand back
//! to a client alongside — or instead of — the original text.
//!
//! Every [`Renderer`] in this crate shares two pieces of infrastructure from
//! [`canvas`]: a small SVG scene builder ([`canvas::Canvas`]) and a
//! rasterizer ([`canvas::rasterize`]) built on a font database seeded with
//! Morph's bundled DejaVu faces, so output is identical on a bare container
//! with no system fonts.
//!
//! Deliberately out of scope for v1 (tracked as future work, not oversights):
//! math/LaTeX rendering and Mermaid/UML diagram rendering. Both would need a
//! real layout/typesetting engine beyond this crate's SVG-composition
//! approach, and neither `ContentKind::Math`, `ContentKind::Mermaid`, nor
//! `ContentKind::Uml` has a `Renderer` here as a result.

pub mod canvas;
pub mod code;
pub mod json;
pub mod log;
pub mod markdown;
pub mod table;

pub use canvas::{rasterize, Canvas, Font, Palette};
pub use code::CodeRenderer;
pub use json::JsonRenderer;
pub use log::LogRenderer;
pub use markdown::MarkdownRenderer;
pub use table::TableRenderer;

use std::sync::Arc;

use morph_core::traits::Renderer;

/// One instance of every renderer this crate provides, in the order a
/// `RepresentationPlanner`/registry would typically want to probe them:
/// structured/exact-text kinds first, prose last. Callers register these
/// (e.g. into a `morph_core::registry::Registry<dyn Renderer>`) rather than
/// constructing renderers themselves, so adding a new renderer here is a
/// one-line change for every caller.
pub fn default_renderers() -> Vec<Arc<dyn Renderer>> {
    vec![
        Arc::new(MarkdownRenderer::new()),
        Arc::new(CodeRenderer::new()),
        Arc::new(JsonRenderer::new()),
        Arc::new(TableRenderer::new()),
        Arc::new(LogRenderer::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::prelude::*;

    #[test]
    fn default_renderers_cover_every_expected_kind() {
        let renderers = default_renderers();
        assert_eq!(renderers.len(), 5);

        let covered = |kind: ContentKind| renderers.iter().any(|r| r.supports(kind));
        assert!(covered(ContentKind::Markdown));
        assert!(covered(ContentKind::Code));
        assert!(covered(ContentKind::Json));
        assert!(covered(ContentKind::Table));
        assert!(covered(ContentKind::TerminalLog));
        assert!(covered(ContentKind::StackTrace));
        // Explicitly out of scope for v1 — see the module doc comment.
        assert!(!covered(ContentKind::Math));
        assert!(!covered(ContentKind::Mermaid));
        assert!(!covered(ContentKind::Uml));
    }
}
