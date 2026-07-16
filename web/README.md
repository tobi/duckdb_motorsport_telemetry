# Telemetry Lab

Local-first browser UI for `duckdb_motorsport_telemetry`.

```sh
bun install
bun run dev
```

For extension loading, place the packaged side modules at:

```text
public/v1.5.4/wasm_eh/motorsport_telemetry.duckdb_extension.wasm
public/v1.5.4/wasm_mvp/motorsport_telemetry.duckdb_extension.wasm
```

The deployed app registers a dropped file with DuckDB-Wasm, loads the Rust extension from the same-origin DuckDB extension repository, detects laps, defaults traces to the best complete lap, exposes interpolated values while scrubbing, queries exact selected-lap samples when a channel is clicked, and provides an unrestricted SQL editor with adaptive query recipes. Files remain in browser memory.

`tests/smoke.mjs` drives headless Chromium through synthetic PDS, MoTeC LD, and VBOX VBO drops and verifies a SQL result. It requires a running Vite preview at port 4173 and `CHROME` pointing to a Chromium executable.
