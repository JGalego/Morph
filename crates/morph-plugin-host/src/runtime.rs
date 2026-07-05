use std::path::Path;
use std::sync::Arc;

use morph_core::error::GatewayError;
use morph_plugin_abi::exports::morph::plugin::plugin::PluginInfo;
use morph_plugin_abi::PluginWorld;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};
use wasmtime_wasi::{p2::add_to_linker_sync, WasiCtxBuilder};

use crate::state::HostState;

/// The only ABI version this build of Morph understands. A plugin compiled
/// against a different version is refused at load time rather than allowed
/// to run and fail unpredictably inside the sandbox.
pub const SUPPORTED_ABI_VERSION: &str = "0.1.0";

/// Per-call CPU budget, expressed in wasmtime fuel units. Chosen generously
/// for real rendering/classification work while still bounding an infinite
/// loop to a bounded wall-clock trap rather than a hang — see
/// `tests::runaway_plugin_is_killed_by_fuel_limit`.
const FUEL_PER_CALL: u64 = 2_000_000_000;

/// Per-instance memory ceiling. Generous for SVG/JSON string building, small
/// enough that a misbehaving plugin can't exhaust host memory.
const MAX_MEMORY_BYTES: usize = 128 * 1024 * 1024;

/// Shared engine + linker, built once and reused across every plugin load
/// and every call. Cheap to clone (`Engine` and `Linker` are both
/// internally `Arc`-based).
#[derive(Clone)]
pub struct PluginRuntime {
    engine: Engine,
    linker: Arc<Linker<HostState>>,
}

impl PluginRuntime {
    pub fn new() -> Result<Self, GatewayError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config)
            .map_err(|e| GatewayError::Plugin(format!("failed to initialize wasm engine: {e}")))?;

        let mut linker = Linker::new(&engine);
        // Structural WASI Preview 2 imports (clocks/random/etc. that the
        // wasip2 target's Rust std pulls in) are linked, but every call gets
        // a WasiCtx with no preopened directories and no inherited
        // stdio/network/env — see `new_store` — so this never grants a
        // plugin real filesystem or network access.
        add_to_linker_sync(&mut linker)
            .map_err(|e| GatewayError::Plugin(format!("failed to link WASI: {e}")))?;

        Ok(PluginRuntime {
            engine,
            linker: Arc::new(linker),
        })
    }

    fn new_store(&self) -> Store<HostState> {
        let wasi = WasiCtxBuilder::new().build();
        // NB: a single logical component instantiation can involve more
        // than one core-wasm instance under the hood (e.g. wit-bindgen
        // adapter shims), so the cap here is a generous sanity bound, not a
        // tight "one plugin, one instance" security boundary — memory and
        // fuel are the limits that actually matter for sandboxing.
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .instances(16)
            .build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                wasi,
                table: ResourceTable::new(),
                limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        // Fuel is refilled to a fixed per-call budget rather than shared
        // across calls, so one expensive call can never starve the next.
        let _ = store.set_fuel(FUEL_PER_CALL);
        store
    }

    /// Loads a `.wasm` component from disk and eagerly calls `manifest()`
    /// once to validate its ABI version, so a bad plugin fails at load time
    /// (visible in `morph plugins` / startup logs) rather than on first use.
    pub fn load(&self, path: &Path) -> Result<LoadedPlugin, GatewayError> {
        let component = Component::from_file(&self.engine, path).map_err(|e| {
            GatewayError::Plugin(format!("failed to load plugin {}: {e}", path.display()))
        })?;

        let mut store = self.new_store();
        let instance =
            PluginWorld::instantiate(&mut store, &component, &self.linker).map_err(|e| {
                GatewayError::Plugin(format!("failed to instantiate {}: {e}", path.display()))
            })?;
        let info = instance
            .morph_plugin_plugin()
            .call_manifest(&mut store)
            .map_err(|e| {
                GatewayError::Plugin(format!(
                    "{} panicked calling manifest(): {e}",
                    path.display()
                ))
            })?;

        if info.abi_version != SUPPORTED_ABI_VERSION {
            return Err(GatewayError::Plugin(format!(
                "plugin {} declares ABI version {} but this build of Morph only supports {}",
                info.name, info.abi_version, SUPPORTED_ABI_VERSION
            )));
        }

        Ok(LoadedPlugin {
            runtime: self.clone(),
            component,
            info,
            path: path.to_path_buf(),
        })
    }
}

/// A validated, ready-to-call plugin. Every call goes through a brand new
/// `Store` (see `PluginRuntime::new_store`) so calls are isolated from each
/// other — no shared mutable state, no fuel/memory carried over between
/// requests.
pub struct LoadedPlugin {
    runtime: PluginRuntime,
    component: Component,
    pub info: PluginInfo,
    path: std::path::PathBuf,
}

impl LoadedPlugin {
    fn instantiate(&self, store: &mut Store<HostState>) -> Result<PluginWorld, GatewayError> {
        PluginWorld::instantiate(store, &self.component, &self.runtime.linker).map_err(|e| {
            GatewayError::Plugin(format!(
                "failed to instantiate {}: {e}",
                self.path.display()
            ))
        })
    }

    fn plugin_error(&self, verb: &str, e: impl std::fmt::Display) -> GatewayError {
        GatewayError::Plugin(format!("plugin {} {verb}: {e}", self.info.name))
    }

    pub fn call_render(&self, input_json: &str) -> Result<String, GatewayError> {
        let mut store = self.runtime.new_store();
        let instance = self.instantiate(&mut store)?;
        let result = instance
            .morph_plugin_plugin()
            .call_render(&mut store, input_json)
            .map_err(|trap| self.plugin_error("trapped in render()", trap))?;
        result.map_err(|msg| self.plugin_error("returned an error from render()", msg))
    }

    pub fn call_classify(&self, text: &str) -> Result<String, GatewayError> {
        let mut store = self.runtime.new_store();
        let instance = self.instantiate(&mut store)?;
        let result = instance
            .morph_plugin_plugin()
            .call_classify(&mut store, text)
            .map_err(|trap| self.plugin_error("trapped in classify()", trap))?;
        result.map_err(|msg| self.plugin_error("returned an error from classify()", msg))
    }

    pub fn call_transform_request(&self, request_json: &str) -> Result<String, GatewayError> {
        let mut store = self.runtime.new_store();
        let instance = self.instantiate(&mut store)?;
        let result = instance
            .morph_plugin_plugin()
            .call_transform_request(&mut store, request_json)
            .map_err(|trap| self.plugin_error("trapped in transform_request()", trap))?;
        result.map_err(|msg| self.plugin_error("returned an error from transform_request()", msg))
    }

    pub fn call_transform_response(&self, response_json: &str) -> Result<String, GatewayError> {
        let mut store = self.runtime.new_store();
        let instance = self.instantiate(&mut store)?;
        let result = instance
            .morph_plugin_plugin()
            .call_transform_response(&mut store, response_json)
            .map_err(|trap| self.plugin_error("trapped in transform_response()", trap))?;
        result.map_err(|msg| self.plugin_error("returned an error from transform_response()", msg))
    }
}
