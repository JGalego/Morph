# Provider adapters

A provider adapter implements exactly one trait —
`morph_core::traits::ProviderAdapter` — and nothing else in the gateway
needs to change to use it.

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    async fn send(&self, req: CanonicalRequest) -> Result<ResponseStream, GatewayError>;
}
```

## Built in

### `openai` (`crates/morph-providers/src/openai.rs`)

Speaks the OpenAI `/chat/completions` wire format. This one adapter, pointed
at a different `base_url`/`api_key`, is what makes all of these work
without a dedicated adapter each — they all speak the same wire format:

Azure OpenAI · Ollama · vLLM · LM Studio · OpenRouter · Together · Groq ·
Cerebras · Mistral · DeepSeek · xAI

```toml
[providers.openai]
kind = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[providers.local-ollama]
kind = "openai"
base_url = "http://localhost:11434/v1"
# no api_key/api_key_env — Ollama's OpenAI-compatible endpoint needs none
```

Always requests `stream: true` from the upstream regardless of what the
client asked for; `morph-gateway` decides independently whether to forward
that stream live or buffer it, based on the client's own request.

### `anthropic` (`crates/morph-providers/src/anthropic.rs`)

Speaks the Anthropic Messages API. Sends `x-api-key` (not `Authorization:
Bearer`) and `anthropic-version: 2023-06-01`.

```toml
[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
```

## Authenticating upstream: your own key, or `passthrough_auth`

By default a provider uses `api_key`/`api_key_env` from `morph.toml` to
authenticate every outbound call — Morph holds the credential, not the
client.

Set `passthrough_auth = true` instead to have Morph forward the client's
*own* request headers (minus a small deny-list of hop-by-hop/body-describing
ones — see `crates/morph-providers/src/util.rs::passthrough_headers`)
verbatim on the upstream call, and ignore `api_key`/`api_key_env` entirely:

```toml
[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
passthrough_auth = true
```

This is what lets Claude Code authenticate through Morph using an
OAuth-backed claude.ai subscription login instead of a separate Anthropic
API key — Morph doesn't need to know which header carries the credential
(it varies: `x-api-key` for a Console key, some combination involving
`authorization`/`anthropic-beta` for a subscription login), it just replays
whatever the client already sent. `morph -- claude` (see the README) uses
this automatically when no Anthropic provider is otherwise configured.

The trade-off: with `passthrough_auth`, Morph itself never validates or
rate-limits by credential — whatever auth reaches Morph reaches the
provider unchanged. Use `[auth]`/`[rate_limit]` in `morph.toml` if you need
Morph itself to gate access.

## Adding a new provider

1. Implement `ProviderAdapter` in a new module (or a new crate, if it has
   its own dependencies you don't want in `morph-providers`).
2. Map `CanonicalRequest` → the provider's native request JSON.
3. Always request streaming from the provider if it supports it — parse
   its native SSE/chunked framing into `ResponseEvent`s
   (`MessageStart`/`TextDelta`/`ToolCallStart`/`ToolCallDelta`/
   `ToolCallEnd`/`Usage`/`MessageStop`). If the provider genuinely can't
   stream, emit one synchronous burst of events instead — the rest of the
   pipeline doesn't need to know the difference.
4. Map non-2xx responses to `GatewayError::Upstream { status, message }`.
5. Register it in `morph-gateway/src/build.rs`'s `match provider_cfg.kind.as_str()`.

Gemini, Bedrock, and Cohere are not implemented — each has a genuinely
distinct wire format from the OpenAI-compatible majority. See
[`ROADMAP.md`](ROADMAP.md).
