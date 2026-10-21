# DuckDB-Wasm adapter (frozen API v1)

The latest published DuckDB-Wasm runtime embeds DuckDB 1.5.4. Native C API v2
artifacts cannot load there. This independent workspace pins the last compatible
adapter to `4eb038b5d7e7cdadc758cd4d4e7e7fbf890e052a` (v1.3.5) as a Git
dependency instead of including the native v2 source. Its parser dependencies
remain at v1.3.5. Native updates cannot silently alter this browser artifact.

Use Rust 1.98.1 and Emscripten 6.0.10, matching the last successful v1 browser
release. Newer toolchains can change libc import signatures such as `lseek`
and produce modules that cannot link against this frozen DuckDB-Wasm runtime.
Activate emsdk and the matching Rust toolchain, then run
`../scripts/build_wasm_extension.sh`. The linker retains
and exports the dependency's `motorsport_telemetry_init_c_api` entrypoint. The
packager explicitly uses `C_STRUCT` and `v1.2.0` API metadata, and publishes only
under the browser runtime's `v1.5.4/wasm_eh` and `v1.5.4/wasm_mvp` paths.
Its extension version stays v1.3.5 independently of the native `VERSION`.

The browser continues to read PDS, LD and VBO files via DuckDB-Wasm's virtual
filesystem. Migrating it to v2 requires an upstream DuckDB-Wasm 2.0 package and
then replacing this frozen adapter, updating its export/footer, and rerunning
both Emscripten builds and the browser smoke tests. CI still tests the v1 lab.
