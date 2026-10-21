#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-"$ROOT/build/wasm"}
VERSION=v1.3.5 # Frozen API-v1 browser adapter, independently of native VERSION.
TARGET=wasm32-unknown-emscripten
ARCHIVE="$ROOT/wasm-extension/target/$TARGET/release/examples/libmotorsport_telemetry.a"
RAW="$OUT/motorsport_telemetry.raw.wasm"

command -v emcc >/dev/null || { echo "emcc is required (activate emsdk)" >&2; exit 1; }
rustup target add "$TARGET" >/dev/null
mkdir -p "$OUT"

cargo build \
  --manifest-path "$ROOT/wasm-extension/Cargo.toml" \
  --target "$TARGET" --release --example motorsport_telemetry

# DuckDB-Wasm 1.5.4 was built with Emscripten's split-i32 libc interface.
# Current Emscripten defaults to BigInt/i64 imports, which cannot link to it.
emcc "$ARCHIVE" \
  -sSIDE_MODULE=2 \
  -sWASM_BIGINT=0 \
  -sEXPORTED_FUNCTIONS=_motorsport_telemetry_init_c_api \
  -O3 -o "$RAW"

for platform in wasm_eh wasm_mvp; do
  mkdir -p "$OUT/$platform"
  python3 "$ROOT/scripts/package_extension.py" \
    --library "$RAW" \
    --output "$OUT/$platform/motorsport_telemetry.duckdb_extension.wasm" \
    --platform "$platform" \
    --api-version v1.2.0 \
    --abi-type C_STRUCT \
    --extension-version "$VERSION"
done

ls -lh "$OUT"/*/motorsport_telemetry.duckdb_extension.wasm
