//! Frozen API-v1 browser extension for DuckDB-Wasm 1.5.4.
//! Native DuckDB 2.0 uses the workspace crate and the v2 entrypoint.

// Keep the upstream exported entrypoint in this static archive. emcc exports
// this symbol when linking the side module.
#[used]
static ENTRYPOINT: unsafe extern "C" fn(
    libduckdb_sys::duckdb_extension_info,
    *const libduckdb_sys::duckdb_extension_access,
) -> bool = telemetry_extension_v1::motorsport_telemetry_init_c_api;
