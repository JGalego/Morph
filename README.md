# Morph

**Morph is a transparent AI gateway.** It sits between any AI client and any
LLM provider, automatically deciding whether a prompt is better sent as
plain text, an image, or both — then forwards it and translates the
response back. Your client never has to change.

```
Your AI App  --->  Morph (localhost:8080)  --->  Any LLM Provider
   (any protocol)         detect → plan → render        (any protocol)
```

## Get started in under a minute

```bash
curl -fsSL https://raw.githubusercontent.com/JGalego/Morph/main/install.sh | sh
export OPENAI_API_KEY=sk-...
morph
```

That's it — no config file to write by hand, nothing else to install. The
first time you run `morph` in a new directory it creates a starter
`morph.toml` for you and tells you what to do next. If something looks
wrong, run `morph doctor` — it checks your config, your API keys, your
listen address, and your plugin setup, and tells you in plain English what
to fix.

Now point your existing AI client at `http://localhost:8080` instead of
your provider's real endpoint. Nothing else about your client needs to
change — Morph speaks its wire protocol already.

No Rust toolchain, no Docker, and no dependencies to install by hand: the
install script above downloads a single static binary for your platform.
Building from source (`cargo build --release`) works too if you prefer it.

## What it actually does

Every request goes through:

1. **Protocol adapter** — recognizes which client wire format arrived
   (OpenAI Chat Completions, Anthropic Messages, or Ollama) and parses it
   into one internal representation.
2. **Content detection** — classifies each message: is this prose,
   Markdown, code, JSON, a table, a terminal log, a stack trace, ...?
3. **Representation planning** — decides, per segment, whether text alone
   is best, or whether rendering it as an image would help the model
   (and if so, whether the image should *accompany* the text or, for
   content where an LLM's own vision transcription would be lossy — JSON,
   code, config — the text always stays, and the image is only ever an
   additive aid, never a replacement).
4. **Rendering** — if warranted, renders the segment (Markdown, code with
   syntax highlighting, a JSON tree, a table, or an ANSI-aware terminal
   log) to SVG/PNG and attaches it to the request.
5. **Provider adapter** — forwards the (possibly enriched) request to your
   real backend in *its* wire format, streams the response back, and
   translates it into whatever format your client is expecting — even if
   that's a different protocol than the one the provider speaks.

Every one of these is a stable Rust trait (`ProtocolAdapter`,
`Renderer`, `ProviderAdapter`, `Classifier`, `RepresentationPlanner`,
`Transformer`). See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the
full pipeline and crate layout.

## Supported today

- **Ingress protocols** (what your client speaks to Morph): OpenAI Chat
  Completions, Anthropic Messages, Ollama.
- **Providers** (what Morph speaks to your LLM): a generic OpenAI-wire
  adapter — which is also what makes Azure OpenAI, Ollama, vLLM, LM
  Studio, OpenRouter, Together, Groq, Cerebras, Mistral, DeepSeek, and xAI
  work, since they all speak the same wire format — plus a native
  Anthropic Messages adapter.
- **Renderers**: Markdown/Documents, Code (syntax highlighted), JSON
  (tree view), Tables, and ANSI-aware terminal logs/stack traces.
- **Plugins**: real, sandboxed WASM plugins (wasmtime, no filesystem/network
  access, CPU and memory limits enforced) can add renderers, classifiers,
  and request/response transformers without recompiling Morph. See
  [`docs/PLUGINS.md`](docs/PLUGINS.md).

What's deliberately **not** attempted yet — see
[`docs/ROADMAP.md`](docs/ROADMAP.md) for why and what the extension points
already support: native Gemini/Bedrock/Cohere adapters (their own wire
formats, distinct from the OpenAI-compatible majority), LaTeX/math
rendering, Mermaid/UML diagram generation, full HTML/CSS rendering, and an
ML-based representation planner (the trait is ready; no model is trained).

## Configuration

One file, `morph.toml`, hot-reloaded — edit it while `morph` is running and
changes to routing/theme/rendering thresholds/cache/rate-limit/auth take
effect immediately, no restart:

```toml
listen = "0.0.0.0:8080"
mode = "auto"              # auto | force_text | force_hybrid
default_provider = "openai"
theme = "dark"
cache = true
stream = true
metrics = true

[providers.openai]
kind = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

See [`morph.example.toml`](morph.example.toml) for every available field,
and run `morph config` to print your current effective configuration.

## CLI

```
morph              start the gateway (also what running with no subcommand does)
morph init          write a starter morph.toml
morph doctor        check your environment/config for common problems
morph config        print the effective configuration
morph providers     list configured providers and their status
morph render FILE    render a Markdown/code/JSON/table/log file standalone (no server)
morph inspect TEXT   dry-run detect+plan against a prompt — see what Morph would do, and why
morph plugins       list loaded WASM plugins
morph test          run a local pipeline smoke test (no network)
morph benchmark     time each built-in renderer
```

## Building from source

```bash
git clone https://github.com/JGalego/Morph
cd Morph
cargo build --release
./target/release/morph
```

Requires Rust 1.85+. Running the full test suite: `cargo test --workspace`.
Building the example WASM plugin requires the `wasm32-wasip2` target:
`rustup target add wasm32-wasip2`.

## Docker

```bash
docker run -p 8080:8080 -e OPENAI_API_KEY=sk-... ghcr.io/jgalego/morph
```

## Documentation

- [Architecture & pipeline](docs/ARCHITECTURE.md)
- [Provider adapters](docs/PROVIDERS.md)
- [Rendering engine](docs/RENDERING.md)
- [Plugin development](docs/PLUGINS.md)
- [Roadmap & known scope boundaries](docs/ROADMAP.md)

## License

Apache-2.0
