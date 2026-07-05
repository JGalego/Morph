# Plugin development

Morph plugins are real WASM components, sandboxed with
[wasmtime](https://wasmtime.dev)'s component model — not a stub. A loaded
plugin gets:

- **No filesystem access.** The host's `WasiCtx` has nothing preopened.
- **No network access.** Nothing inherited, nothing granted.
- **A fixed CPU budget.** Every call gets a fresh fuel allowance
  (`wasmtime`'s fuel metering); an infinite loop traps instead of hanging
  the host — see `crates/morph-plugin-host/tests/example_plugin.rs::
  runaway_plugin_is_killed_by_fuel_limit_instead_of_hanging` for a real,
  passing test of this.
- **A fixed memory ceiling** (128 MiB by default).

Provider adapters are deliberately **not** part of the plugin surface —
they hold API keys and make outbound network calls, which is exactly the
capability a sandboxed plugin must not have. Plugins can extend
**renderers**, **classifiers**, and **request/response transformers** —
the pipeline stages that are pure computation and therefore safe to run
untrusted.

## The interface (`crates/morph-plugin-abi/wit/plugin.wit`)

One shared, small functional shape — JSON/bytes in, JSON/bytes out —
rather than a bespoke ABI per plugin kind:

```wit
interface plugin {
    record plugin-info {
        name: string,
        version: string,
        kind: string,           // "renderer" | "classifier" | "transformer"
        abi-version: string,
        supported-kinds: list<string>,   // renderer plugins only
    }

    manifest: func() -> plugin-info;
    render: func(input-json: string) -> result<string, string>;
    classify: func(text: string) -> result<string, string>;
    transform-request: func(request-json: string) -> result<string, string>;
    transform-response: func(response-json: string) -> result<string, string>;
}
```

The host only ever calls the function(s) matching a plugin's declared
`kind` — implement the others to return an `Err` describing that, as the
example plugin does.

### Renderer wire shape

`render(input_json)` receives:
```json
{"kind": "plain_text", "raw": "...", "metrics": {...}, "language": null,
 "options": {"theme": "dark", "max_width_px": 1200, "scale": 1.0, "format": "svg"}}
```
and must return:
```json
{"mime": "image/svg+xml", "bytes_base64": "...", "width": 480, "height": 220, "alt_text": "..."}
```
(`bytes` is base64-encoded since a WIT `string` must be valid UTF-8.)

### Classifier wire shape

`classify(text)` returns a JSON array of `[kind, confidence]` pairs — the
same shape as `Vec<(ContentKind, f32)>` serializes to, most confident
first: `[["json", 0.92], ["markdown", 0.3]]`.

### Transformer wire shape

`transform_request`/`transform_response` receive and return the
`serde_json` encoding of `CanonicalRequest`/`CanonicalResponse`.

## Writing a plugin (Rust)

See `plugins/example-renderer` for a complete, working example — it's a
**separate Cargo workspace** (nested under the main one but deliberately
not a member of it, since it targets `wasm32-wasip2` and has nothing to do
with the native build):

```bash
rustup target add wasm32-wasip2
cd plugins/example-renderer
cargo build --target wasm32-wasip2 --release
# -> target/wasm32-wasip2/release/example_renderer.wasm
```

The guest side uses `wit_bindgen::generate!` pointed at the same
`wit/plugin.wit` file the host uses, so host and guest can never drift out
of sync silently — a breaking interface change fails both builds instead
of failing at runtime inside the sandbox.

```rust
wit_bindgen::generate!({ world: "plugin-world", path: "../../crates/morph-plugin-abi/wit" });

struct Component;
impl exports::morph::plugin::plugin::Guest for Component {
    fn manifest() -> exports::morph::plugin::plugin::PluginInfo { /* ... */ }
    fn render(input_json: String) -> Result<String, String> { /* ... */ }
    fn classify(_: String) -> Result<String, String> { Err("not a classifier".into()) }
    fn transform_request(_: String) -> Result<String, String> { Err("not a transformer".into()) }
    fn transform_response(_: String) -> Result<String, String> { Err("not a transformer".into()) }
}
export!(Component);
```

Nothing about this ABI is Rust-specific — any language with a WASM
Component Model toolchain (the ecosystem is still young, but growing) can
speak it.

## Loading plugins

```toml
[plugins]
enabled = true
dir = "./plugins-enabled"   # any directory of *.wasm component files
```

Drop compiled `.wasm` files into that directory and restart Morph (plugin
discovery happens at startup — see `docs/ARCHITECTURE.md` for what's fixed
at startup vs. hot-reloadable). `morph plugins` lists what's loaded, and
`morph doctor` reports any that failed to load and why.

## ABI versioning

`plugin-info.abi-version` is checked against
`morph_plugin_host::SUPPORTED_ABI_VERSION` at load time — a plugin built
against an incompatible version is refused with a clear error rather than
loaded and allowed to fail unpredictably.
