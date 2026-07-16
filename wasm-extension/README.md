# DuckDB-Wasm adapter

This standalone Cargo project compiles the same vectorized DuckDB adapter and Rust format parsers into an Emscripten static library. `scripts/build_wasm_extension.sh` links that archive as a DuckDB-Wasm side module and packages `wasm_eh` and `wasm_mvp` variants.

The separate manifest is intentional: native DuckDB 1.4.3 and DuckDB-Wasm's DuckDB 1.5.4 C APIs both link `libduckdb`, so Cargo cannot resolve them in one dependency graph. Parser source remains shared.

```sh
source /path/to/emsdk/emsdk_env.sh
../scripts/build_wasm_extension.sh
```

The browser registers dropped files in DuckDB-Wasm's virtual filesystem. At bind time, the extension reads those bytes through DuckDB's C VFS API and passes them to `CosworthFile::from_bytes`, `MotecFile::from_bytes`, or `VboFile::from_bytes`.
