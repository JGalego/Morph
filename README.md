<p align="center">
  <img src="docs/assets/banner.gif" alt="Morph — the transparent AI gateway" width="600">
</p>

<p align="center">
  <a href="https://github.com/JGalego/Morph/actions/workflows/ci.yml"><img src="https://github.com/JGalego/Morph/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/JGalego/Morph/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.94%2B-orange" alt="Rust 1.94+"></a>
  <a href="https://github.com/JGalego/Morph/stargazers"><img src="https://img.shields.io/github/stars/JGalego/Morph" alt="Stars"></a>
</p>

Morph is a transparent AI gateway: it sits between your AI client and your LLM provider, and converts every prompt into whatever format the model reads best — text, an image, or both. No client-side changes needed.

```
Your AI App  --->  Morph (localhost:8080) ● ▸ ■ ▸ ▲  --->  Any LLM Provider
```

## Quickstart

```bash
curl -fsSL https://raw.githubusercontent.com/JGalego/Morph/main/install.sh | sh
morph
```

Point your client's API base URL at `http://localhost:8080` — nothing else changes. First run in a new directory auto-creates `morph.toml`. Something wrong? `morph doctor`.

## Tutorial: turn every prompt into an image

[DeepSeek OCR](https://arxiv.org/abs/2510.18234) proved that images can hold text more efficiently than text itself. Here's how Morph puts that to work.

First, scaffold a config:

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

JSON, code, and config stay exact text; only prose, Markdown, tables, and logs get rendered as images (see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the exact rule). This needs a vision-capable model on the other end — a text-only local model will reject the image.

See it happening: set `[inspector] enabled = true` and open
`http://localhost:8080/_inspector` for every request side by side with its rendered image, or run `morph inspect "your text" --save-images ./out` to render one prompt to disk with no server needed.

No Anthropic API key on hand? If Claude Code is logged into a [claude.ai](https://claude.ai) subscription, passthrough auth forwards its credential instead of needing one:

```toml
[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
passthrough_auth = true
```

## How it works

Every request is scanned for its content — prose, code, JSON, a table, a log — then Morph decides per segment whether text, an image, or both will serve the model best, renders accordingly, forwards it to the real provider in its wire format, and translates the response back, even across protocols.

Every stage is a swappable Rust trait — see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Supported

| | |
|---|---|
| **Clients → Morph** | OpenAI Chat Completions, Anthropic Messages, Ollama |
| **Morph → Providers** | OpenAI-wire (OpenAI, Azure, and 10 more OpenAI-compatible providers — full list in [`docs/PROVIDERS.md`](docs/PROVIDERS.md)) and native Anthropic |
| **Renderers** | Markdown/documents, syntax-highlighted code, JSON trees, tables, ANSI-aware logs |
| **Plugins** | Sandboxed WASM (wasmtime) — add renderers/classifiers/transformers, no recompile. [`docs/PLUGINS.md`](docs/PLUGINS.md) |

## Configuration

Everything lives in `morph.toml`, hot-reloaded so edits apply without restarting:

```toml
mode = "auto"              # auto | force_text | force_hybrid | force_image_only
default_provider = "openai"

[providers.openai]
kind = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

Full field list: [`morph.example.toml`](morph.example.toml). Current effective config: `morph config`.

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

`morph -- <command>` sets every common base-URL env var at once (`ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OLLAMA_HOST`); `--env NAME=VALUE` covers a one-off nonstandard value, and `[exec] extra_base_url_env_vars` in `morph.toml` covers a var name you want pointed at Morph every time.

## Build & run

Prefer to build from source instead of the install script? Clone and compile:

```bash
git clone https://github.com/JGalego/Morph && cd Morph
cargo build --release && ./target/release/morph
```

Needs Rust 1.94+; run `cargo test --workspace` for the full suite, and `rustup target add wasm32-wasip2` if you want the example WASM plugin to build too.

Or skip the build and run it in Docker instead:

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
