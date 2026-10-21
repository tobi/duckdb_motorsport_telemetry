#!/usr/bin/env bash
# Exercise v2 loading, catalog docs and vector semantics in a real DuckDB core.
set -euo pipefail
: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to the native v2 extension}"
repo="$(mktemp -d)"
trap 'rm -rf "$repo"' EXIT
"$DUCKDB" -no-agent -unsigned -bail -c "
SET extension_directory='$repo';
INSTALL AND LOAD '$EXTENSION';
SELECT CASE WHEN telemetry_convert(10.0, 'm/s', 'km/h') = 36.0 THEN true ELSE error('constant scalar conversion') END;
SELECT CASE WHEN telemetry_convert(NULL::DOUBLE, 'm/s', 'km/h') IS NULL AND telemetry_convert(1.0, NULL::VARCHAR, 'km/h') IS NULL AND telemetry_can_convert(NULL::VARCHAR, 'km/h') IS NULL THEN true ELSE error('scalar NULL propagation') END;
SELECT CASE WHEN (SELECT count(*) FROM (SELECT telemetry_convert(i::DOUBLE, 'm/s', 'km/h') AS value FROM range(4097) t(i)) WHERE value >= 0) = 4097 THEN true ELSE error('multi-chunk conversion') END;
SELECT CASE WHEN (SELECT list(telemetry_convert(value, unit, 'km/h') ORDER BY id) FROM (VALUES (1, 10.0, 'm/s'),(2, NULL, 'm/s'),(3, 20.0, 'km/h')) t(id,value,unit)) IS NOT DISTINCT FROM [36.0,NULL,20.0] THEN true ELSE error('mixed vector conversion') END;
SELECT CASE WHEN (SELECT count(*) FROM duckdb_functions() WHERE function_name IN ('telemetry_metadata','telemetry_file_metadata','telemetry_session_metadata','telemetry_laps','telemetry_samples','read_telemetry','read_telemetry_session','read_aim','read_aimd','read_cosworth','read_motec','read_vbo','read_telemetry_normalized','write_telemetry','telemetry_column_comments','telemetry_units','telemetry_convert','telemetry_convert_column','telemetry_can_convert') AND length(description)>0 AND length(examples)>0 AND list_contains(categories,'telemetry')) = 19 THEN true ELSE error('function documentation incomplete') END;
SELECT CASE WHEN (SELECT parameters[1] FROM duckdb_functions() WHERE function_name='telemetry_metadata') = 'path' THEN true ELSE error('positional parameter name') END;
" >/dev/null
# Loading the installed extension by name must work in a new connection.
"$DUCKDB" -no-agent -unsigned -bail -c "SET extension_directory='$repo'; LOAD motorsport_telemetry; SELECT * FROM telemetry_units() LIMIT 1;" >/dev/null
for invalid in "telemetry_convert(1.0,'m/s','bar')" "telemetry_convert_column(1.0,'km/h')"; do
  if "$DUCKDB" -no-agent -unsigned -bail -c "LOAD '$EXTENSION'; SELECT $invalid;" >/dev/null 2>&1; then
    printf 'invalid conversion accepted: %s\n' "$invalid" >&2
    exit 1
  fi
done
printf 'C API v2 integration tests passed\n'
