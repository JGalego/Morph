//! Sandboxed WASM plugin host for Morph, built on wasmtime's component
//! model. Loads plugins that implement the interface in
//! `morph-plugin-abi`/`wit/plugin.wit` and exposes them as ordinary
//! `morph_core::traits::{Renderer, Classifier, Transformer}` trait objects —
//! the rest of the gateway never needs to know a given renderer is running
//! in a sandbox rather than natively compiled in.
//!
//! Every plugin call runs in a fresh `Store` with: no filesystem access, no
//! network access (a `WasiCtx` with nothing preopened/inherited), a fixed
//! fuel budget (traps a runaway/infinite-looping plugin instead of hanging),
//! and a fixed memory ceiling (traps a plugin that tries to allocate
//! unbounded memory). Provider adapters are deliberately outside this
//! surface — see the module docs in `runtime` and the WIT file for why.

mod adapters;
mod discover;
mod runtime;
mod state;

pub use adapters::{WasmClassifier, WasmRenderer, WasmTransformer};
pub use discover::{load_plugins_from_dir, DiscoveredPlugins};
pub use runtime::{LoadedPlugin, PluginRuntime, SUPPORTED_ABI_VERSION};
