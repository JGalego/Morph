//! Host-side bindings generated from `wit/plugin.wit`, the single source of
//! truth for Morph's WASM plugin ABI. Both `morph-plugin-host` (via this
//! crate) and guest plugins (via `wit_bindgen::generate!` pointed directly
//! at `wit/plugin.wit`) build against the same interface definition, so a
//! guest and this host binding can never drift out of sync silently — a
//! breaking WIT change fails both sides' builds instead of failing at
//! runtime inside a sandboxed instance.
//!
//! See `wit/plugin.wit` for why the ABI is one small shared functional
//! shape rather than one bespoke interface per plugin kind.

wasmtime::component::bindgen!({
    path: "wit",
    world: "plugin-world",
});
