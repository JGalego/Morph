use wasmtime::component::ResourceTable;
use wasmtime::StoreLimits;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// Per-`Store` state. One of these is created fresh for every plugin call
/// (see `runtime::call`), so a call's resource limits/fuel never leak into
/// the next call.
pub struct HostState {
    pub(crate) wasi: WasiCtx,
    pub(crate) table: ResourceTable,
    pub(crate) limits: StoreLimits,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
