# Morph

A transparent AI gateway. It sits between your AI client and your LLM
provider, automatically turning prompts into whatever format the model
handles best — plain text, an image, or both — with zero client-side
changes.

```
Your AI App  --->  Morph (localhost:8080)  --->  Any LLM Provider
```

## Quickstart

```bash
curl -fsSL https://raw.githubusercontent.com/JGalego/Morph/main/install.sh | sh
morph
```

Point your client's API base URL at `http://localhost:8080` — nothing else
changes. First run in a new directory auto-creates `morph.toml`. Something
wrong? `morph doctor`.

## Tutorial: turn every prompt into an image

The core trick, hands-on — every prompt gets rendered before the model
sees it.

```bash
morph init
```

Edit `morph.toml`:

```toml
mode = "force_image_only"
```

Then run your client through Morph:

```bash
morph -- claude      # or: morph, then point any client at localhost:8080
```

Content that must stay exact — JSON, code, config — keeps its text
regardless; only prose, Markdown, tables, and logs get replaced (see
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the exact rule). This
needs a vision-capable model on the other end — a text-only local model
will reject the image.

Two ways to see it happening, no guessing required:

- **Live**: set `[inspector] enabled = true`, open
  `http://localhost:8080/_inspector` — every request, side by side, with
  the actual rendered image inline.
- **Offline, for one prompt**: `morph inspect "your text" --save-images
  ./out` — writes the exact PNG Morph would send. No server needed.

No Anthropic API key on hand? If Claude Code is logged into a claude.ai
subscription, use passthrough auth instead of a key:

```toml
[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
passthrough_auth = true
```

Morph then forwards whatever credential Claude Code already sends,
instead of needing its own.

## How it works

Every request: detect content (prose? code? JSON? a table? a log?) →
decide per-segment whether text, an image, or both serves the model best
→ render if so → forward to the real provider in its wire format →
translate the response back, even across protocols. Every stage is a
swappable Rust trait — see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Supported

| | |
|---|---|
| **Clients speak to Morph via** | OpenAI Chat Completions, Anthropic Messages, Ollama |
| **Morph speaks to providers via** | OpenAI-wire (covers OpenAI, Azure, Ollama, vLLM, LM Studio, OpenRouter, Together, Groq, Cerebras, Mistral, DeepSeek, xAI) and native Anthropic |
| **Renderers** | Markdown/documents, syntax-highlighted code, JSON trees, tables, ANSI-aware logs |
| **Plugins** | Sandboxed WASM (wasmtime) — add renderers/classifiers/transformers, no recompile. [`docs/PLUGINS.md`](docs/PLUGINS.md) |

Deliberately not attempted yet (and why): [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Configuration

One file, hot-reloaded — edits apply without restarting:

```toml
mode = "auto"              # auto | force_text | force_hybrid | force_image_only
default_provider = "openai"

[providers.openai]
kind = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

Full field list: [`morph.example.toml`](morph.example.toml). Current
effective config: `morph config`.

## CLI

```
morph                 start the gateway
morph -- <command>    start the gateway, then run <command> against it (any AI CLI — Claude Code, Cursor, ...)
morph init            write a starter morph.toml
morph doctor          diagnose config/environment problems
morph inspect TEXT    preview the detect→plan decision for a prompt (add --save-images to render it)
morph render FILE     render a file standalone, no server
morph providers       list configured providers
morph plugins         list loaded WASM plugins
morph config          print effective configuration
morph test            local pipeline smoke test
morph benchmark       time each renderer
```

`morph -- <command>` sets every common base-URL env var at once
(`ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `OPENAI_API_BASE`,
`OLLAMA_HOST`); `--env NAME=VALUE` covers anything nonstandard.

## Build & run

```bash
git clone https://github.com/JGalego/Morph && cd Morph
cargo build --release && ./target/release/morph
```

Requires Rust 1.85+. `cargo test --workspace` for the full suite. The
example WASM plugin needs `rustup target add wasm32-wasip2`.

```bash
docker run -p 8080:8080 -e OPENAI_API_KEY=sk-... ghcr.io/jgalego/morph
```

## Docs

[Architecture](docs/ARCHITECTURE.md) ·
[Providers](docs/PROVIDERS.md) ·
[Rendering](docs/RENDERING.md) ·
[Plugins](docs/PLUGINS.md) ·
[Roadmap](docs/ROADMAP.md)

## License

Apache-2.0
