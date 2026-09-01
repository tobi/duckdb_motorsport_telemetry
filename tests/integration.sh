#!/usr/bin/env bash
set -euo pipefail

: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to motorsport_telemetry.duckdb_extension}"
fixture_dir="$(mktemp -d)"
fixture="$fixture_dir/synthetic.pds"
motec_fixture="$fixture_dir/synthetic.ld"
vbo_fixture="$fixture_dir/synthetic.vbo"
aim_fixture="$fixture_dir/synthetic.mp4"
aim_fixture_2="$fixture_dir/synthetic_part2.mp4"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Upstream fixtures: a local symlink straight to motorsport-telemetry-rs/tests/fixtures,
# or CI's sparse checkout of that repository (which keeps its tests/fixtures/ prefix).
fixtures="$root_dir/tests/fixtures/telemetry-rs"
[[ -d "$fixtures/tests/fixtures" ]] && fixtures="$fixtures/tests/fixtures"
cp "$fixtures/synthetic_aimd.mp4" "$aim_fixture"
cp "$fixtures/synthetic_aimd_part2.mp4" "$aim_fixture_2"
cp "$fixtures/synthetic_cosworth.pds" "$fixture"
cp "$fixtures/synthetic_motec.ld" "$motec_fixture"
cp "$fixtures/synthetic_vbo.vbo" "$vbo_fixture"
trap 'rm -rf "$fixture_dir"' EXIT
[[ -n "${KEEP_FIXTURES:-}" ]] && trap - EXIT


out_ld="$fixture_dir/roundtrip.ld"
mapped_ld="$fixture_dir/mapped.ld"

sql="LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$fixture') WHERE name='Speed') = 4081 THEN true ELSE error('bad channel metadata') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$fixture')) = 'pds' THEN true ELSE error('format detection failed') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$fixture', channel='Speed') ORDER BY sample_index LIMIT 4)) = [13.1, 14.2, 15.3, 16.4] THEN true ELSE error('chunk order was not preserved') END;
SELECT CASE WHEN (SELECT list(round(\"Speed\", 6) ORDER BY time_ns) FROM (SELECT * FROM read_telemetry('$fixture', rate=1, channels='Speed') ORDER BY time_ns LIMIT 3)) = [13.1, 18.6, 24.1] THEN true ELSE error('wide scan failed') END;
SELECT CASE WHEN (SELECT list(round(telemetry_convert_column(\"Speed\", 'km/h'), 6) ORDER BY time_ns) FROM (SELECT * FROM read_telemetry('$fixture', rate=1, channels='Speed', unit_tags=true) ORDER BY time_ns LIMIT 3)) = [47.16, 66.96, 86.76] THEN true ELSE error('tagged column conversion failed') END;
SELECT CASE WHEN (SELECT list(round(\"Speed\", 6) ORDER BY time_ns) FROM (SELECT * FROM read_telemetry('$fixture', rate=1) ORDER BY time_ns LIMIT 3)) = [13.1, 18.6, 24.1] THEN true ELSE error('all-channels default failed') END;
SELECT CASE WHEN (SELECT list(round(\"Speed\", 6) ORDER BY time_ns) FROM read_telemetry('$fixture', rate=10, channels='Speed', end_ns=600000000)) = [13.1, 13.65, 14.2, 14.75, 15.3, 15.85] THEN true ELSE error('mixed-rate interpolation failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', filename=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename option failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', add_filename_as_column=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename alias failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$motec_fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('MoTeC parser failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$vbo_fixture', channel='velocity kmh')) = [10.0, 20.0, 30.0, 40.0] THEN true ELSE error('VBO parser failed') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$fixture') WHERE name IN ('Speed','Throttle Pos','Brake Pedal Pos','G_FORCE_LAT','G_FORCE_LONG','Lap Distance','Lap Number','GPS Latitude','GPS Longitude') AND sample_count=4081) = 9 THEN true ELSE error('PDS important channels incomplete') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$motec_fixture') WHERE name IN ('Speed','Throttle Pos','Brake Pedal Pos','G_FORCE_LAT','G_FORCE_LONG','Lap Distance','Lap Number','GPS Latitude','GPS Longitude') AND sample_count=4) = 9 THEN true ELSE error('MoTeC important channels incomplete') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$vbo_fixture') WHERE name IN ('velocity kmh','throttle','brake','gforce_lat','gforce_long','distance','lap','latitude','longitude') AND sample_count=4) = 9 THEN true ELSE error('VBO important channels incomplete') END;
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$aim_fixture') WHERE name='RPM') = 1 THEN true ELSE error('AiM metadata failed') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$aim_fixture')) = 'aimd' THEN true ELSE error('AiM format detection failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$aim_fixture', channel='RPM')) = [1234.5] THEN true ELSE error('AiM scalar decode failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_aim('$aim_fixture', channels='RPM', rate=10)) = 1 THEN true ELSE error('AiM wide reader failed') END;
SELECT CASE WHEN (SELECT round(value, 6) FROM telemetry_samples('$aim_fixture', channel='GPS Speed')) = 0.137477 THEN true ELSE error('AiM GPS aggregate decode failed') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_metadata('$aim_fixture') WHERE name LIKE 'GPS %' AND sample_count=1) = 16 THEN true ELSE error('AiM GPS channel set incomplete') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*')) = ['aimd', 'motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format glob failed') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*.{pds,ld,vbo,mp4}')) = ['aimd', 'motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format brace glob failed') END;
SELECT CASE WHEN (SELECT count(DISTINCT source_file) FROM read_telemetry_session('$fixture_dir/*.mp4', rate=10, channels='RPM,GPS Speed', max_gap_seconds=1)) = 2 THEN true ELSE error('multi-file Aim session grouping failed') END;
SELECT CASE WHEN (SELECT [video_frame_index,driver_id,lap_number] FROM read_telemetry_session('$aim_fixture', rate=10, channels='RPM') LIMIT 1) = [2,3,1] THEN true ELSE error('Aim video frame/session context failed') END;
SELECT CASE WHEN (SELECT [count(*),min(time_ns),min(file_time_ns)] FROM read_telemetry_session('$fixture_dir/*.mp4', rate=10, channels='RPM', start_ns=150000000, end_ns=300000000, max_gap_seconds=1)) = [1,200000000,0] THEN true ELSE error('session-relative pruning failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_telemetry_session('$vbo_fixture', rate=2, channels='velocity kmh') WHERE video_file_index=0 AND video_sync_time=0 AND video_frame_index IS NULL) = 4 THEN true ELSE error('VBO video linkage failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_cosworth('$fixture', channels='Speed', rate=1)) = 817 THEN true ELSE error('read_cosworth failed') END;
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
SELECT CASE WHEN (SELECT samples FROM write_telemetry('$fixture', '$out_ld')) = 40810 THEN true ELSE error('write_telemetry sample count wrong') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$out_ld', channel='Speed') ORDER BY sample_index LIMIT 4)) = [13.1, 14.2, 15.3, 16.4] THEN true ELSE error('LD round-trip lost data') END;
-- SQL-native export mappings use lists, the unit registry, and derived sums
SELECT CASE WHEN (SELECT [channels, samples] FROM write_telemetry('$fixture', '$mapped_ld', channel_mapping=[['Speed','Ground Speed','km/h']], sum_channels=[['Speed','Speed','Double Speed','m/s']])) = [2, 8162] THEN true ELSE error('SQL-native mapped export shape failed') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$mapped_ld', channel='Ground Speed') ORDER BY sample_index LIMIT 4)) = [47.16, 51.12, 55.08, 59.04] THEN true ELSE error('automatic export unit conversion failed') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$mapped_ld', channel='Double Speed') ORDER BY sample_index LIMIT 4)) = [26.2, 28.4, 30.6, 32.8] THEN true ELSE error('derived sum export failed') END;
-- channel_map renames, converts, and reports the mapped unit as declared
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6') ORDER BY sample_index LIMIT 4)) = [47.16, 51.12, 55.08, 59.04] THEN true ELSE error('channel_map conversion failed') END;
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = ['Ground Speed', 'km/h', 'declared'] THEN true ELSE error('channel_map metadata failed') END;
-- an offset-only rule applies without a scale
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> S +-1') ORDER BY sample_index LIMIT 4)) = [12.1, 13.2, 14.3, 15.4] THEN true ELSE error('offset-only rule failed') END;
-- the wide reader renames and converts its columns too
SELECT CASE WHEN (SELECT list(round(\"Ground Speed\", 6) ORDER BY time_ns) FROM (SELECT * FROM read_telemetry('$fixture', rate=1, channels='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6') ORDER BY time_ns LIMIT 3)) = [47.16, 66.96, 86.76] THEN true ELSE error('wide channel_map failed') END;
-- unmapped channels pass through untouched
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle Pos -> Pedal [%] *0.01')) = ['Speed', 'm/s', 'declared'] THEN true ELSE error('unmapped channel metadata changed') END;
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM (SELECT * FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle Pos -> Pedal [%] *0.01') ORDER BY sample_index LIMIT 4)) = [13.1, 14.2, 15.3, 16.4] THEN true ELSE error('unmapped channel was modified') END;
-- column comment DDL is generated and correctly quoted
SELECT CASE WHEN (SELECT count(*) FROM telemetry_column_comments('$motec_fixture', 'laps')) > 0 THEN true ELSE error('no column comments generated') END;
SELECT CASE WHEN (SELECT ddl FROM telemetry_column_comments('$motec_fixture', 'laps') WHERE column_name='Speed') LIKE 'COMMENT ON COLUMN %laps%.%Speed% IS ''unit=%' THEN true ELSE error('column comment DDL malformed') END;
SELECT CASE WHEN (SELECT kv_metadata FROM telemetry_column_comments('$fixture', 'laps') WHERE column_name='Throttle Pos') LIKE '%native_frequency_hz=5; native_sample_period_ns=200000000' THEN true ELSE error('native sample rate missing from column metadata') END;"
results="$("$DUCKDB" -unsigned -csv -noheader -c "$sql")"
[[ "$(grep -c '^true$' <<<"$results")" = 49 ]]

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

# `.telemetry` (zstd MTJ by default, legacy zip still readable) and the
# stint-aware lap table. The MTJ is produced by the upstream CLI when
# available; otherwise the extension's own write path is not involved, so
# skip quietly.
if command -v motorsport-telemetry >/dev/null 2>&1; then
  mtj="$fixture_dir/synthetic.telemetry"
  legacy="$fixture_dir/legacy.telemetry"
  motorsport-telemetry convert --no-passes "$fixture" "$mtj" >/dev/null 2>&1
  motorsport-telemetry convert --no-passes --native-zip "$fixture" "$legacy" >/dev/null 2>&1
  "$DUCKDB" -unsigned -c "LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT count(*) FROM telemetry_laps('$fixture')) = 5 THEN true ELSE error('telemetry_laps on pds failed') END;
SELECT CASE WHEN (SELECT list(kind ORDER BY lap_number) FROM telemetry_laps('$fixture')) = ['out','flying','flying','flying','in'] THEN true ELSE error('lap kinds wrong') END;
SELECT CASE WHEN (SELECT list(label ORDER BY lap_number) FROM telemetry_laps('$fixture')) = ['S1 out','S1 L2','S1 L3','S1 L4','S1 in'] THEN true ELSE error('lap labels wrong') END;
SELECT CASE WHEN (SELECT flying_lap_count FROM telemetry_file_metadata('$fixture')) = 3 THEN true ELSE error('flying_lap_count wrong') END;
SELECT CASE WHEN (SELECT fastest_lap_label FROM telemetry_file_metadata('$fixture')) = 'S1 L2' THEN true ELSE error('fastest_lap_label wrong') END;
SELECT CASE WHEN (SELECT list(kind ORDER BY lap_number) FROM telemetry_laps('$mtj')) = (SELECT list(kind ORDER BY lap_number) FROM telemetry_laps('$fixture')) THEN true ELSE error('.telemetry (zstd MTJ) laps differ from source') END;
SELECT CASE WHEN (SELECT list(kind ORDER BY lap_number) FROM telemetry_laps('$legacy')) = (SELECT list(kind ORDER BY lap_number) FROM telemetry_laps('$fixture')) THEN true ELSE error('.telemetry (legacy zip) laps differ from source') END;
SELECT CASE WHEN (SELECT source_format FROM telemetry_file_metadata('$mtj')) = 'pds' THEN true ELSE error('source_format not carried by .telemetry') END;
SELECT CASE WHEN (SELECT count(*) FROM telemetry_file_metadata('$fixture_dir/*.{pds,telemetry}')) = 3 THEN true ELSE error('brace expansion with telemetry failed') END;
SELECT CASE WHEN (SELECT round(max(\"Speed\"), 3) FROM read_telemetry('$mtj', rate=1, channels='Speed')) = (SELECT round(max(\"Speed\"), 3) FROM read_telemetry('$fixture', rate=1, channels='Speed')) THEN true ELSE error('.telemetry wide read differs') END;
" >/dev/null
else
  printf 'motorsport-telemetry CLI not found; skipping .telemetry container checks\n' >&2
fi

# read_telemetry_normalized: the blessed channels with fixed units, plus the
# lap model, one row per instant. The synthetic PDS has Speed in m/s and
# pedals in %, five laps (out, 3 flying, in).
"$DUCKDB" -unsigned -c "LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT count(*) FROM read_telemetry_normalized('$fixture', rate=1)) = 817 THEN true ELSE error('normalized row count') END;
SELECT CASE WHEN (SELECT list(DISTINCT lap_kind ORDER BY lap_kind) FROM read_telemetry_normalized('$fixture', rate=1) WHERE lap_kind IS NOT NULL) = ['flying','in','out'] THEN true ELSE error('normalized lap kinds') END;
SELECT CASE WHEN (SELECT max(lap_number) FROM read_telemetry_normalized('$fixture', rate=1)) = 5 THEN true ELSE error('normalized virtual lap number') END;
SELECT CASE WHEN (SELECT round(max(speed_mps), 3) FROM read_telemetry_normalized('$fixture', rate=1)) = (SELECT round(max(\"Speed\"), 3) FROM read_telemetry('$fixture', rate=1, channels='Speed')) THEN true ELSE error('normalized speed differs from source m/s') END;
SELECT CASE WHEN (SELECT max(throttle_fraction) FROM read_telemetry_normalized('$fixture', rate=1)) <= 1.0 THEN true ELSE error('throttle_fraction not 0..1') END;
SELECT CASE WHEN (SELECT count(*) FROM read_telemetry_normalized('$fixture', rate=1) WHERE lap_progress < 0 OR lap_progress > 1) = 0 THEN true ELSE error('lap_progress out of range') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry_normalized('$fixture', rate=1, filename=true) LIMIT 1) = '$fixture' THEN true ELSE error('normalized filename option') END;
SELECT CASE WHEN (SELECT count(*) FROM read_telemetry_normalized('$fixture', rate=10, start_ns=10000000000, end_ns=20000000000)) = 100 THEN true ELSE error('normalized start/end pruning') END;
" >/dev/null

stats="$(python3 scripts/telemetry_stats.py "$fixture" --extension "$EXTENSION" --duckdb "$DUCKDB" --rate 2 --channels Speed)"
grep -q '^Raw mixed-rate sample stats$' <<<"$stats"
grep -q '^Interpolated wide stats at 2 Hz$' <<<"$stats"
grep -q "$(basename "$fixture")" <<<"$stats"

printf 'integration tests passed\n'
