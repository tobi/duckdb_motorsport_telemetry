#!/usr/bin/env bash
set -euo pipefail

: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to motorsport_telemetry.duckdb_extension}"
fixture_dir="$(mktemp -d)"
fixture="$fixture_dir/synthetic.pds"
motec_fixture="$fixture_dir/synthetic.ld"
vbo_fixture="$fixture_dir/synthetic.vbo"
trap 'rm -rf "$fixture_dir"' EXIT
[[ -n "${KEEP_FIXTURES:-}" ]] && trap - EXIT

python3 - "$fixture" "$motec_fixture" "$vbo_fixture" <<'PY'
import struct, sys
p, motec_path, vbo_path = sys.argv[1:]
data = bytearray(0x700)
def u32(o, v): struct.pack_into('<I', data, o, v)
def utf16(o, text, size): data[o:o+len(text.encode('utf-16le'))] = text.encode('utf-16le')[:size]
def directory(o, section, count, class_a, class_b, next_count):
    u32(o, section); u32(o + 8, count); u32(o + 0x10, class_a)
    u32(o + 0x14, class_b); u32(o + 0x18, next_count)
def definition(o, channel_id, name, unit):
    u32(o, channel_id); utf16(o + 8, name, 112); utf16(o + 0x90, unit, 32)  # unit field lives at +0x90
def chunk(o, order, channel_id, ptr):
    u32(o, order); u32(o + 4, channel_id); u32(o + 8, channel_id)
    u32(o + 0x18, 10_000_000); u32(o + 0x1c, 2); u32(o + 0x38, ptr)

defs, width = 0x200, 0xc0
chunks, chunk_width = defs + width * 2, 0x40
end = chunks + chunk_width * 4
directory(0x80, defs, 2, 8, 1, 4)
directory(0xa0, chunks, 4, 1, 3, 0)
directory(0xc0, end, 0, 1, 1, 0)
definition(defs, 1, 'Speed', 'm/s')
definition(defs + width, 2, 'Throttle', '%')
for i, (order, channel, values) in enumerate([
    (100, 1, (10.0, 11.0)), (200, 2, (0.0, 25.0)),
    (1, 1, (12.0, 13.0)), (2, 2, (50.0, 75.0)),
]):
    ptr = 0x580 + i * 0x20
    chunk(chunks + i * chunk_width, order, channel, ptr)
    struct.pack_into('<2d', data, ptr, *values)
open(p, 'wb').write(data)

motec = bytearray(0x400)
struct.pack_into('<I', motec, 0, 0x40)
struct.pack_into('<I', motec, 0x08, 0x200)
struct.pack_into('<I', motec, 0x208, 0x300)
struct.pack_into('<I', motec, 0x20c, 4)
struct.pack_into('<H', motec, 0x212, 0x07)
struct.pack_into('<H', motec, 0x214, 4)
struct.pack_into('<H', motec, 0x216, 2)
motec[0x220:0x225] = b'Speed'
# Unit lives at +0x48 in the channel block (0x200), i.e. 0x248. 0x240 is the
# short-name field, so writing the unit there leaves the unit empty.
motec[0x248:0x24b] = b'm/s'
struct.pack_into('<4f', motec, 0x300, 1, 2, 3, 4)
open(motec_path, 'wb').write(motec)

open(vbo_path, 'w').write('''[header]\ntime\nvelocity kmh\n[column names]\ntime velocity\n[data]\n120000.0 10\n120000.5 20\n120001.0 30\n120001.5 40\n''')
PY

out_ld="$fixture_dir/roundtrip.ld"

sql="LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$fixture') WHERE name='Speed') = 4 THEN true ELSE error('bad channel metadata') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$fixture')) = 'pds' THEN true ELSE error('format detection failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('chunk order was not preserved') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('wide scan failed') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1)) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('all-channels default failed') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=2, channels='Speed', end_ns=3000000000)) = [10.0, 10.5, 11.0, 11.5, 12.0, 12.5] THEN true ELSE error('mixed-rate interpolation failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', filename=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename option failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', add_filename_as_column=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename alias failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$motec_fixture', channel='Speed')) = [1.0, 2.0, 3.0, 4.0] THEN true ELSE error('MoTeC parser failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$vbo_fixture', channel='velocity kmh')) = [10.0, 20.0, 30.0, 40.0] THEN true ELSE error('VBO parser failed') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*')) = ['motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format glob failed') END;
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
SELECT CASE WHEN (SELECT samples FROM write_telemetry('$fixture', '$out_ld')) = 8 THEN true ELSE error('write_telemetry sample count wrong') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$out_ld', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('LD round-trip lost data') END;
-- channel_map renames, converts, and reports the mapped unit as declared
SELECT CASE WHEN (SELECT list(round(value, 6) ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('channel_map conversion failed') END;
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = ['Ground Speed', 'km/h', 'declared'] THEN true ELSE error('channel_map metadata failed') END;
-- an offset-only rule applies without a scale
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Speed -> S +-1')) = [9.0, 10.0, 11.0, 12.0] THEN true ELSE error('offset-only rule failed') END;
-- the wide reader renames and converts its columns too
SELECT CASE WHEN (SELECT list(round(\"Ground Speed\", 6) ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed', channel_map='Speed -> Ground Speed [km/h] *3.6')) = [36.0, 39.6, 43.2, 46.8] THEN true ELSE error('wide channel_map failed') END;
-- unmapped channels pass through untouched
SELECT CASE WHEN (SELECT DISTINCT [channel, unit, unit_source] FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle -> Pedal [%] *0.01')) = ['Speed', 'm/s', 'declared'] THEN true ELSE error('unmapped channel metadata changed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed', channel_map='Throttle -> Pedal [%] *0.01')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('unmapped channel was modified') END;
-- column comment DDL is generated and correctly quoted
SELECT CASE WHEN (SELECT count(*) FROM telemetry_column_comments('$motec_fixture', 'laps')) > 0 THEN true ELSE error('no column comments generated') END;
SELECT CASE WHEN (SELECT ddl FROM telemetry_column_comments('$motec_fixture', 'laps') WHERE column_name='Speed') LIKE 'COMMENT ON COLUMN %laps%.%Speed% IS ''unit=%' THEN true ELSE error('column comment DDL malformed') END;"
results="$("$DUCKDB" -unsigned -csv -noheader -c "$sql")"
[[ "$(grep -c '^true$' <<<"$results")" = 30 ]]
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
