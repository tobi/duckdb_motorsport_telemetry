#!/usr/bin/env bash
# End-to-end proof: read a PDS with a channel map, materialise it, attach unit
# comments, and read the metadata back out of DuckDB's catalog.
#
# The rules below are a team's mapping, the kind that would live in version
# control. Rules whose source channel is absent from the given file are dropped
# before use, so this runs against any PDS rather than one specific export.
set -euo pipefail

EXT="${EXT:-./build/release/motorsport_telemetry.duckdb_extension}"
PDS="${1:?usage: unit_metadata_demo.sh FILE.pds}"
MAP="$(mktemp /tmp/channel_map.XXXXXX)"
CANDIDATES="$(mktemp /tmp/channel_rules.XXXXXX)"
DB="$(mktemp -u /tmp/telemetry.XXXXXX.duckdb)"
trap 'rm -f "$MAP" "$CANDIDATES" "$DB"' EXIT

# Cosworth PDS stores SI. These rules convert to the units i2 expects.
#
# Hand-written scale factors are exactly where unit bugs hide: an earlier
# version of this file used *1 for rad/s -> rpm and reported a 286 km/h lap at
# 889 rpm. Cross-check any factor here against telemetry_convert(), which
# derives it from the registry:
#   SELECT telemetry_convert(1, 'rad/s', 'rpm');  -- 9.549296585513721
cat >"$CANDIDATES" <<'RULES'
Speed_Ref    -> Ground Speed  [km/h] *3.6
STEER        -> Steered Angle [deg]  *57.29577951308232
P_F_BRAKE    -> Brake Press   [bar]  *0.00001
I_ACCEL_LONG -> G Force Long  [g]    *0.10197162129779283
X_FL_DAMPER  -> Damper FL     [mm]   *1000
RPM          -> Engine Speed  [rpm]  *9.549296585513721
ACT          -> Air Temp      [C]    +-273.15
RULES

# Keep only rules whose source channel exists in this file and holds samples.
: >"$MAP"
while IFS= read -r rule; do
  [[ -z "${rule// }" || "$rule" == \#* ]] && continue
  source_channel="${rule%%->*}"
  source_channel="${source_channel%"${source_channel##*[![:space:]]}"}"
  escaped="${source_channel//\'/\'\'}"
  present="$(duckdb -unsigned -noheader -list -c "
    LOAD '${EXT}';
    SELECT count(*) FROM telemetry_metadata('${PDS}')
    WHERE name = '${escaped}' AND sample_count > 0;")"
  [[ "$present" == "0" ]] || printf '%s\n' "$rule" >>"$MAP"
done <"$CANDIDATES"

mapped_count="$(grep -c . "$MAP" || true)"
if [[ "$mapped_count" == "0" ]]; then
  printf 'none of the demo rules match channels in %s\n' "$PDS" >&2
  exit 1
fi
printf 'applying %s of %s rules that match this file:\n' \
  "$mapped_count" "$(grep -c . "$CANDIDATES")"
cat "$MAP"
echo

# The table holds only the mapped channels, so scope the generated DDL to the
# same set via `channels`. Without it the function would emit COMMENT
# statements for every channel in the file, naming columns the table lacks.
sources="$(sed 's/->.*//; s/[[:space:]]*$//' "$MAP" | paste -sd, -)"

COMMENTS="$(duckdb -unsigned -noheader -list "$DB" -c "
LOAD '${EXT}';
SELECT string_agg(ddl, ' ' ORDER BY column_name)
FROM telemetry_column_comments('${PDS}', 'laps',
                               channel_map := '${MAP}', channels := '${sources}');
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

echo '--- mapped values, with the mapped unit from the catalog ---'
duckdb -unsigned "$DB" -c "
LOAD '${EXT}';
SELECT c.column_name,
       regexp_extract(c.comment, 'unit=([^;]*)', 1) AS unit,
       regexp_extract(c.comment, 'dimension=([^;]*)', 1) AS dimension
FROM duckdb_columns() c
WHERE c.table_name = 'laps' AND c.comment IS NOT NULL
ORDER BY c.column_name;
"

# Verify each mapped column's range in its own declared unit, so the check
# adapts to whichever rules applied instead of hardcoding column names.
echo '--- mapped column ranges (in the unit the map declared) ---'
while IFS= read -r rule; do
  target="${rule#*-> }"
  target="${target%%[*}"
  target="${target%"${target##*[![:space:]]}"}"
  unit="${rule#*[}"
  unit="${unit%%]*}"
  escaped_target="${target//\"/\"\"}"
  duckdb -unsigned -noheader -list "$DB" -c "
    LOAD '${EXT}';
    SELECT '${target} [${unit}]: ' ||
           printf('%.2f', min(\"${escaped_target}\")) || ' .. ' ||
           printf('%.2f', max(\"${escaped_target}\"))
    FROM laps;"
done <"$MAP"
