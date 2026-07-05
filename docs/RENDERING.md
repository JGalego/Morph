# Rendering engine

`crates/morph-render` turns a `DetectedContent` segment into a
`RenderedAsset` (SVG bytes, or a rasterized PNG). Every renderer implements:

```rust
pub trait Renderer: Send + Sync {
    fn name(&self) -> &str;
    fn supports(&self, kind: ContentKind) -> bool;
    fn render(&self, content: &DetectedContent, opts: &RenderOptions) -> Result<RenderedAsset, GatewayError>;
}
```

## Built-in renderers

| Renderer | `ContentKind` | Notes |
|---|---|---|
| `MarkdownRenderer` | `Markdown` | Also doubles as the "Document" renderer — headings with a size hierarchy, bullet/ordered lists, pipe-tables, fenced code blocks (delegates to `CodeRenderer`), paragraphs. Block-level only; inline emphasis (`**bold**`, links) renders as literal text. |
| `CodeRenderer` | `Code` | Syntax highlighting via `syntect` (pure-Rust regex backend, no Oniguruma), line-number gutter, falls back to plain tokenization for an unrecognized language. |
| `JsonRenderer` | `Json` | Indented tree layout, keys/strings/numbers/bools/null each colored distinctly, faint per-depth indent guides. |
| `TableRenderer` | `Table` | Markdown pipe-tables (CSV as a fallback), header/alternating-row backgrounds, column widths sized to content. |
| `LogRenderer` | `TerminalLog`, `StackTrace` | Parses ANSI SGR escape codes into styled spans; without ANSI codes, heuristically highlights timestamps/`ERROR`/`WARN`/stack-frame lines. |

## Why pure-Rust rendering

`usvg`/`resvg`/`tiny-skia` for SVG authoring and rasterization, and
`syntect` built with its `regex-fancy` (pure-Rust) backend instead of
Oniguruma — no system font, cairo, or C-library dependency. This is what
keeps a static/musl binary possible: rendering works identically on a bare
container with no fonts installed, because the five DejaVu font files
(`assets/fonts/`, permissive license — see `assets/fonts/README.md`) are
embedded directly into the binary via `include_bytes!`, not loaded from
the system.

## Adding a renderer

Implement `Renderer`, add it to `morph_render::default_renderers()` (or
register it separately if it lives in its own crate), and it's available
to the pipeline immediately — `RepresentationPlanner` doesn't need to know
renderers exist; `morph-gateway`'s representation stage just looks up
`state.renderers.iter().find(|r| r.supports(kind))`.

For rendering logic that should be sandboxed, or that you don't want to
recompile Morph to add, write a WASM plugin instead — see
[`PLUGINS.md`](PLUGINS.md).

## Deliberately out of scope for v1

Math/LaTeX and Mermaid/UML diagram generation both need a real
layout/typesetting engine beyond this crate's SVG-composition approach, and
neither `ContentKind::Math`, `ContentKind::Mermaid`, nor `ContentKind::Uml`
has a renderer as a result — see [`ROADMAP.md`](ROADMAP.md). Full HTML/CSS
rendering (a real browser/layout engine) is similarly out of scope.
