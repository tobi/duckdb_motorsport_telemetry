#!/usr/bin/env bash
set -euo pipefail

: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to motorsport_telemetry.duckdb_extension}"
fixture_dir="$(mktemp -d)"
fixture="$fixture_dir/synthetic.pds"
motec_fixture="$fixture_dir/synthetic.ld"
vbo_fixture="$fixture_dir/synthetic.vbo"
aim_fixture="$fixture_dir/synthetic.mp4"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cp "$root_dir/tests/fixtures/synthetic_aimd.mp4" "$aim_fixture"
cp "$root_dir/tests/fixtures/synthetic_cosworth.pds" "$fixture"
cp "$root_dir/tests/fixtures/synthetic_motec.ld" "$motec_fixture"
cp "$root_dir/tests/fixtures/synthetic_vbo.vbo" "$vbo_fixture"
trap 'rm -rf "$fixture_dir"' EXIT
[[ -n "${KEEP_FIXTURES:-}" ]] && trap - EXIT


out_ld="$fixture_dir/roundtrip.ld"
mapped_ld="$fixture_dir/mapped.ld"

sql="LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$fixture') WHERE name='Speed') = 4 THEN true ELSE error('bad channel metadata') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$fixture')) = 'pds' THEN true ELSE error('format detection failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('chunk order was not preserved') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('wide scan failed') END;
SELECT CASE WHEN (SELECT list(round(telemetry_convert_column(\"Speed\", 'km/h'), 6) ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed', unit_tags=true)) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('tagged column conversion failed') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1)) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('all-channels default failed') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=2, channels='Speed', end_ns=3000000000)) = [10.0, 10.5, 11.0, 11.5, 12.0, 12.5] THEN true ELSE error('mixed-rate interpolation failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', filename=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename option failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', add_filename_as_column=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename alias failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$motec_fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('MoTeC parser failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$vbo_fixture', channel='velocity kmh')) = [10.0, 20.0, 30.0, 40.0] THEN true ELSE error('VBO parser failed') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$fixture') WHERE name IN ('Speed','Throttle Pos','Brake Pedal Pos','G_FORCE_LAT','G_FORCE_LONG','Lap Distance','Lap Number','GPS Latitude','GPS Longitude') AND sample_count=4) = 9 THEN true ELSE error('PDS important channels incomplete') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$motec_fixture') WHERE name IN ('Speed','Throttle Pos','Brake Pedal Pos','G_FORCE_LAT','G_FORCE_LONG','Lap Distance','Lap Number','GPS Latitude','GPS Longitude') AND sample_count=4) = 9 THEN true ELSE error('MoTeC important channels incomplete') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$vbo_fixture') WHERE name IN ('velocity kmh','throttle','brake','gforce_lat','gforce_long','distance','lap','latitude','longitude') AND sample_count=4) = 9 THEN true ELSE error('VBO important channels incomplete') END;
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$aim_fixture') WHERE name='RPM') = 1 THEN true ELSE error('AiM metadata failed') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$aim_fixture')) = 'aimd' THEN true ELSE error('AiM format detection failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$aim_fixture', channel='RPM')) = [1234.5] THEN true ELSE error('AiM scalar decode failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_aim('$aim_fixture', channels='RPM', rate=10)) = 1 THEN true ELSE error('AiM wide reader failed') END;
SELECT CASE WHEN (SELECT round(value, 6) FROM telemetry_samples('$aim_fixture', channel='GPS Speed')) = 0.137477 THEN true ELSE error('AiM GPS aggregate decode failed') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$aim_fixture') WHERE name LIKE 'GPS %' AND sample_count=1) = 15 THEN true ELSE error('AiM GPS channel set incomplete') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*')) = ['aimd', 'motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format glob failed') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*.{pds,ld,vbo,mp4}')) = ['aimd', 'motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format brace glob failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_cosworth('$fixture', channels='Speed', rate=1)) = 4 THEN true ELSE error('read_cosworth failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_motec('$motec_fixture', channels='Speed', rate=2)) = 4 THEN true ELSE error('read_motec failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_vbo('$vbo_fixture', channels='velocity kmh', rate=2)) = 4 THEN true ELSE error('read_vbo failed') END;
SELECT CASE WHEN (SELECT typeof(create_date) FROM read_telemetry('$fixture', channels='Speed', add_create_date_as_column=true, create_date_from=TIMESTAMP '1970-01-01', create_date_to=TIMESTAMP '2100-01-01') LIMIT 1) = 'TIMESTAMP' THEN true ELSE error('create date column failed') END;
SELECT CASE WHEN (SELECT typeof(modified_at) FROM read_telemetry('$fixture', channels='Speed', add_modified_at_as_column=true) LIMIT 1) = 'TIMESTAMP' THEN true ELSE error('modified-at column failed') END;
SELECT CASE WHEN (SELECT [typeof(create_date), typeof(modified_at)] FROM read_telemetry('$fixture', channels='Speed', timestamps=true) LIMIT 1) = ['TIMESTAMP', 'TIMESTAMP'] THEN true ELSE error('timestamps option failed') END;
-- unit provenance is exposed and only ever takes known values
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$fixture') WHERE unit_source NOT IN ('declared','spec_default','unknown')) = 0 THEN true ELSE error('bad unit_source value') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_samples('$fixture', channel='Speed') WHERE unit_source NOT IN ('declared','spec_default','unknown')) = 0 THEN true ELSE error('bad sample unit_source') END;
-- a MoTeC file declares its units, so provenance must be 'declared'
SELECT CASE WHEN (SELECT unit_source FROM telemetry_metadata('$motec_fixture') WHERE name='Speed') = 'declared' THEN true ELSE error('MoTeC declared unit not detected') END;
-- write_telemetry round-trips values bit-for-bit
SELECT CASE WHEN (SELECT samples FROM write_telemetry('$fixture', '$out_ld')) = 36 THEN true ELSE error('write_telemetry sample count wrong') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$out_ld', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('LD round-trip lost data') END;
-- SQL-native export mappings use lists, the unit registry, and derived sums
SELECT CASE WHEN (SELECT [channels, samples] FROM write_telemetry('$fixture', '$mapped_ld', channel_mapping=[['Speed','Ground Speed','km/h']], sum_channels=[['Speed','Speed','Double Speed','m/s']])) = [2, 8] THEN true ELSE error('SQL-native mapped export shape failed') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM telemetry_samples('$mapped_ld', channel='Ground Speed')) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('automatic export unit conversion failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$mapped_ld', channel='Double Speed')) = [20.0, 22.0, 24.0, 26.0] THEN true ELSE error('derived sum export failed') END;
-- channel_map renames, converts, and reports the mapped unit as declared
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('channel_map conversion failed') END;
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = ['Ground Speed', 'km/h', 'declared'] THEN true ELSE error('channel_map metadata failed') END;
-- an offset-only rule applies without a scale
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> S +-1')) = [9.0, 10.0, 11.0, 12.0] THEN true ELSE error('offset-only rule failed') END;
-- the wide reader renames and converts its columns too
SELECT CASE WHEN (SELECT list(round(\"Ground Speed\", 6) ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('wide channel_map failed') END;
-- unmapped channels pass through untouched
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle Pos -> Pedal [%] *0.01')) = ['Speed', 'm/s', 'declared'] THEN true ELSE error('unmapped channel metadata changed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle Pos -> Pedal [%] *0.01')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('unmapped channel was modified') END;
-- column comment DDL is generated and correctly quoted
SELECT CASE WHEN (SELECT count(*) FROM telemetry_column_comments('$motec_fixture', 'laps')) > 0 THEN true ELSE error('no column comments generated') END;
SELECT CASE WHEN (SELECT ddl FROM telemetry_column_comments('$motec_fixture', 'laps') WHERE column_name='Speed') LIKE 'COMMENT ON COLUMN %laps%.%Speed% IS ''unit=%' THEN true ELSE error('column comment DDL malformed') END;
SELECT CASE WHEN (SELECT kv_metadata FROM telemetry_column_comments('$fixture', 'laps') WHERE column_name='Throttle Pos') LIKE '%native_frequency_hz=1; native_sample_period_ns=1000000000' THEN true ELSE error('native sample rate missing from column metadata') END;"
results="$("$DUCKDB" -unsigned -csv -noheader -c "$sql")"
[[ "$(grep -c '^true$' <<<"$results")" = 45 ]]

# Inferred conversion is intentionally restricted to direct unit-tagged reader
# columns. Scalars and expressions must use telemetry_convert(value, from, to).
for expression in "1.0" 'CAST("Speed" AS DOUBLE)'; do
  from_clause=""
  [[ "$expression" != "1.0" ]] && from_clause=" FROM read_telemetry('$fixture', channels='Speed', unit_tags=true)"
  if "$DUCKDB" -unsigned -no-stdin -c "LOAD '$EXTENSION'; SELECT telemetry_convert_column($expression, 'km/h')$from_clause LIMIT 1;" >/dev/null 2>&1; then
    printf 'telemetry_convert_column accepted untagged expression %s\n' "$expression" >&2
    exit 1
  fi
done

sidecar="${out_ld%.ld}.ldx"
[[ -s "$sidecar" ]]
python3 - "$sidecar" <<'PY'
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
assert root.tag == 'LDXFile'
PY

# Malformed and typo'd channel_map rules must fail at bind time rather than
# silently doing nothing, which would mean silently unconverted data.
for bad_map in 'Nope -> X *2' 'Speed -> X *zzz'; do
  if "$DUCKDB" -unsigned -c "LOAD '$EXTENSION';
SELECT * FROM telemetry_samples('$fixture', channel_map='$bad_map');" >/dev/null 2>&1; then
    printf 'channel_map rule %s was not rejected\n' "$bad_map" >&2
    exit 1
  fi
done

stats="$(python3 scripts/telemetry_stats.py "$fixture" --extension "$EXTENSION" --duckdb "$DUCKDB" --rate 2 --channels Speed)"
grep -q '^Raw mixed-rate sample stats$' <<<"$stats"
grep -q '^Interpolated wide stats at 2 Hz$' <<<"$stats"
grep -q "$(basename "$fixture")" <<<"$stats"

printf 'integration tests passed\n'
