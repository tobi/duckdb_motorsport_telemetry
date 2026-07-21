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

The deployed app registers a dropped or public example file with DuckDB-Wasm, loads the Rust extension from the same-origin DuckDB extension repository, detects laps, defaults traces to the best complete lap, overlays comparison laps by distance, synchronizes trace and GPS-map scrubbing, reports unit-normalized performance headlines, queries exact selected-lap samples when a channel is clicked, and provides an unrestricted SQL editor with adaptive query recipes. Signal-role overrides are saved for matching channel layouts. Files remain in browser memory.

On startup, the demo automatically fetches an immutable, attributed Lamborghini GT3 Barcelona MoTeC fixture directly from the Apache-2.0-licensed [JBonifay/motec-file-parser](https://github.com/JBonifay/motec-file-parser) repository. The SHA-256 digest is verified and the file is cached in the browser after its first download.

`tests/smoke.mjs` drives headless Chromium through synthetic PDS, MoTeC LD, and VBOX VBO drops and verifies a SQL result. It requires a running Vite preview at port 4173 and `CHROME` pointing to a Chromium executable.
