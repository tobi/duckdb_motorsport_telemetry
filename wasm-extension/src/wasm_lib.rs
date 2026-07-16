//! Emscripten static library linked as a DuckDB-Wasm side module.
#![allow(special_module_name)]

#[path = "../../crates/duckdb-motorsport-telemetry/src/lib.rs"]
mod extension;
