#!/usr/bin/env bash
# End-to-end proof: read a PDS with a channel map, materialise it, attach unit
# comments, and read the metadata back out of DuckDB's catalog.
set -euo pipefail

EXT="${EXT:-./build/release/motorsport_telemetry.duckdb_extension}"
PDS="${1:?usage: unit_metadata_demo.sh FILE.pds}"
MAP="$(mktemp /tmp/channel_map.XXXXXX)"
DB="$(mktemp -u /tmp/telemetry.XXXXXX.duckdb)"
trap 'rm -f "$MAP" "$DB"' EXIT

# A team's mapping, the kind that would live in version control.
cat >"$MAP" <<'RULES'
# Cosworth PDS stores SI. These rules convert to the units i2 expects.
Speed_Ref    -> Ground Speed  [km/h] *3.6
STEER        -> Steered Angle [deg]  *57.29577951308232
P_F_BRAKE    -> Brake Press   [bar]  *0.00001
I_ACCEL_LONG -> G Force Long  [g]    *0.10197162129779283
X_FL_DAMPER  -> Damper FL     [mm]   *1000
RULES

# Generate the COMMENT statements from the file's own unit metadata.
COMMENTS="$(duckdb -unsigned -noheader -list "$DB" -c "
LOAD '${EXT}';
SELECT string_agg(ddl, ' ' ORDER BY column_name)
FROM telemetry_column_comments('${PDS}', 'laps', channel_map := '${MAP}');
")"

# Materialise the table and attach the comments.
duckdb -unsigned "$DB" -c "
LOAD '${EXT}';
CREATE TABLE laps AS
  SELECT * FROM read_telemetry('${PDS}', channel_map := '${MAP}', rate := 20);
${COMMENTS}
" >/dev/null

echo '--- unit metadata read back from the DuckDB catalog ---'
duckdb "$DB" -c "
SELECT column_name, comment
FROM duckdb_columns()
WHERE table_name = 'laps' AND comment IS NOT NULL
ORDER BY column_name;
"

echo '--- converted values are physically sensible ---'
duckdb "$DB" -c "
SELECT
  printf('%.1f', max(\"Ground Speed\"))  AS top_speed_kmh,
  printf('%.1f', min(\"Steered Angle\")) AS steer_min_deg,
  printf('%.1f', max(\"Brake Press\"))   AS brake_max_bar,
  printf('%.2f', min(\"G Force Long\"))  AS braking_g,
  printf('%.1f', max(\"Damper FL\"))     AS damper_max_mm
FROM laps;
"
