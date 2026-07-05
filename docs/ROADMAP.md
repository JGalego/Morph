# Roadmap & known scope boundaries

This is a from-scratch v0.1. The sections below are things deliberately
**not** attempted yet, and — for each — why, and what already exists in
the architecture to make adding it later a mechanical "implement the
trait" exercise rather than a redesign.

## Native Gemini / Bedrock / Cohere provider adapters

The generic `openai` adapter already covers the large majority of the
named ecosystem (Azure OpenAI, Ollama, vLLM, LM Studio, OpenRouter,
Together, Groq, Cerebras, Mistral, DeepSeek, xAI) because they all speak
the OpenAI chat/completions wire format. Gemini, Bedrock, and Cohere each
have a genuinely distinct wire format. Adding one is: implement
`ProviderAdapter` (see [`PROVIDERS.md`](PROVIDERS.md)), register it in
`morph-gateway/src/build.rs`. No other crate changes.

## Native Gemini ingress protocol

Same story on the client-facing side — `ProtocolAdapter` is the trait,
`morph-protocols` is the crate, `morph-gateway`'s router is where a new
route gets added.

## Math/LaTeX rendering

Needs a real math typesetting engine (glyph-level layout, not just SVG
composition). `ContentKind::Math` exists in `morph-core`; nothing renders
it yet. A `MathRenderer` implementing `Renderer` — native or, given it's
pure computation, as a WASM plugin — is a self-contained addition.

## Mermaid/UML/auto-generated diagrams

Same shape as math: needs a real graph-layout algorithm (even a basic
layered/force-directed one) that this crate's SVG-composition approach
doesn't attempt yet. `ContentKind::Mermaid`/`ContentKind::Uml` exist;
detection for them isn't implemented in `morph-detect` either.

## Full HTML/CSS rendering

Needs an actual browser/layout engine (or a serious subset like
`litehtml`), not achievable with hand-rolled SVG composition. Given the
existing renderers already cover the content types that dominate real LLM
traffic (Markdown, code, JSON, tables, logs), this was deprioritized in
favor of depth on those.

## ML-based `RepresentationPlanner`

`RepresentationPlanner` is a trait for exactly this reason —
`morph_detect::DefaultPlanner` is a fixed heuristic implementation, and
`morph_core::stats::StatsSink` / `morph_middleware::InMemoryStatsSink`
already record `(content_kind, representation) → (latency, tokens, cost,
success)` outcomes on every request. A future ML-based planner reads that
same aggregate data and implements the same trait; `morph-gateway` doesn't
change at all — it already calls the planner through the trait, not a
concrete type.

## Multi-provider request routing

Today, one running Morph instance forwards every request to whichever
provider `default_provider` names — matching the project's own example
config (`provider = "openai"`, a single value) and the "any client, one
target LLM" framing. `morph-gateway`'s `Registry<dyn ProviderAdapter>`
already supports registering multiple providers simultaneously; routing
by, say, a model-name prefix instead of always `default_provider` is a
change contained entirely to `pipeline::handle`.

## Native/WASI HTTP inside plugins

Provider-adapter-as-plugin was considered and deliberately rejected, not
deferred — see [`PLUGINS.md`](PLUGINS.md) and the security notes in
[`ARCHITECTURE.md`](ARCHITECTURE.md). Giving sandboxed guest code control
over outbound network calls carrying API keys is a security boundary,
not a missing feature.

## Streaming-safe response transformation

`Transformer::transform_response` only runs on the buffered path today —
see the "why" in [`ARCHITECTURE.md`](ARCHITECTURE.md). A streaming-safe
version would need a sliding window over recent output to catch patterns
spanning a chunk boundary; not implemented in v1.

## HTTP/3

The server is built on `axum`/`hyper` (HTTP/1.1 and HTTP/2 today). HTTP/3
would mean integrating `quinn`/`h3`, whose ecosystem is still less mature
relative to the rest of this stack; not attempted in v1.

## CI-verified cross-platform release builds

`.github/workflows/release.yml` is written and should cross-compile
Linux/macOS/Windows binaries on real GitHub-hosted runners, but it hasn't
been (and can't be, from this environment) executed against real runners
to confirm it works end-to-end. Treat it as a strong starting point, not
a guarantee, until it's actually run once.
