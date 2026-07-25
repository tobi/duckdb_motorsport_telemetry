#!/usr/bin/env bash
# End-to-end proof that units reach a materialised table AND a Parquet file.
#
# Neither carrier alone is enough: column comments live in the DuckDB catalog
# but are dropped by COPY ... TO parquet, while Parquet KV_METADATA survives
# export but cannot annotate a DuckDB table. This exercises both, from the
# file's own declared units, with no hand-written unit strings.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="${EXT:-$REPO_ROOT/build/release/motorsport_telemetry.duckdb_extension}"
DUCKDB="${DUCKDB:-duckdb}"
SRC="${1:?usage: units_metadata_e2e.sh TELEMETRY_FILE}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
parquet="$work/run.parquet"

# Pick two channels that actually declare a unit, so the test proves real
# provenance rather than a spec default.
mapfile -t chans < <("$DUCKDB" -unsigned -noheader -list -c "
  LOAD '$EXT';
  SELECT name FROM telemetry_metadata('$SRC')
  WHERE unit_source = 'declared' AND sample_count > 0 AND canonical_unit IS NOT NULL
  ORDER BY name LIMIT 2;")
if [[ ${#chans[@]} -lt 2 ]]; then
  printf 'need 2 channels with declared units in %s, found %s\n' "$SRC" "${#chans[@]}" >&2
  exit 1
fi
list="${chans[0]},${chans[1]}"
printf 'channels under test: %s\n' "$list"

"$DUCKDB" -unsigned <<SQL
LOAD '$EXT';

CREATE TABLE run AS
  SELECT * FROM read_cosworth('$SRC', rate:=5, channels:='$list');

-- 1. Apply catalog comments generated from the file's own units.
CREATE TABLE ddl AS
  SELECT ddl FROM telemetry_column_comments('$SRC', 'run')
  WHERE column_name IN ('${chans[0]}', '${chans[1]}');
SELECT 'applying ' || count(*) || ' comment statement(s)' AS step FROM ddl;

.mode list
.headers off
.once $work/apply.sql
SELECT ddl FROM ddl;
SQL

"$DUCKDB" -unsigned <<SQL
LOAD '$EXT';
CREATE TABLE run AS
  SELECT * FROM read_cosworth('$SRC', rate:=5, channels:='$list');
.read $work/apply.sql

-- Verify the comment is readable back out of the catalog.
SELECT column_name, comment AS catalog_comment
FROM duckdb_columns() WHERE table_name = 'run' ORDER BY column_name;

-- 2. Export to Parquet with KV_METADATA so units survive the file boundary.
COPY run TO '$parquet' (FORMAT PARQUET, KV_METADATA {
  '${chans[0]}': '$("$DUCKDB" -unsigned -noheader -list -c "LOAD '$EXT'; SELECT kv_metadata FROM telemetry_column_comments('$SRC','run') WHERE column_name = '${chans[0]}';")',
  '${chans[1]}': '$("$DUCKDB" -unsigned -noheader -list -c "LOAD '$EXT'; SELECT kv_metadata FROM telemetry_column_comments('$SRC','run') WHERE column_name = '${chans[1]}';")'
});

-- 3. Read the units back out of the Parquet file itself.
SELECT key::VARCHAR AS column_name, value::VARCHAR AS parquet_metadata
FROM parquet_kv_metadata('$parquet') ORDER BY column_name;

-- 4. Use the recovered unit to convert, proving the metadata is actionable.
SELECT
  regexp_extract(value::VARCHAR, 'unit=([^;]*)', 1) AS unit,
  regexp_extract(value::VARCHAR, 'dimension=([^;]*)', 1) AS dimension,
  telemetry_can_convert(
    regexp_extract(value::VARCHAR, 'unit=([^;]*)', 1), 'km/h') AS to_kmh_ok
FROM parquet_kv_metadata('$parquet') ORDER BY 1;
SQL

# Hard assertion: the comment must be present in the catalog, and the unit
# must be recoverable from the Parquet file.
commented="$("$DUCKDB" -unsigned -noheader -list <<SQL
LOAD '$EXT';
CREATE TABLE run AS SELECT * FROM read_cosworth('$SRC', rate:=5, channels:='$list');
.read $work/apply.sql
SELECT count(*) FROM duckdb_columns()
WHERE table_name = 'run' AND comment LIKE 'unit=%dimension=%';
SQL
)"
in_parquet="$("$DUCKDB" -unsigned -noheader -list -c \
  "SELECT count(*) FROM parquet_kv_metadata('$parquet') WHERE value::VARCHAR LIKE 'unit=%';")"

printf '\ncolumns with unit in catalog comment: %s\n' "$commented"
printf 'columns with unit in parquet metadata: %s\n' "$in_parquet"
if [[ "$commented" -ge 2 && "$in_parquet" -ge 2 ]]; then
  printf 'PASS: units reached both the DuckDB catalog and the Parquet file\n'
else
  printf 'FAIL: units did not reach both carriers\n' >&2
  exit 1
fi
