#!/usr/bin/env bash
set -euo pipefail

: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to motorsport_telemetry.duckdb_extension}"
fixture_dir="$(mktemp -d)"
fixture="$fixture_dir/synthetic.pds"
motec_fixture="$fixture_dir/synthetic.ld"
vbo_fixture="$fixture_dir/synthetic.vbo"
trap 'rm -rf "$fixture_dir"' EXIT

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
    u32(o, channel_id); utf16(o + 8, name, 112); utf16(o + 0x98, unit, 32)
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
motec[0x240:0x243] = b'm/s'
struct.pack_into('<4f', motec, 0x300, 1, 2, 3, 4)
open(motec_path, 'wb').write(motec)

open(vbo_path, 'w').write('''[header]\ntime\nvelocity kmh\n[column names]\ntime velocity\n[data]\n120000.0 10\n120000.5 20\n120001.0 30\n120001.5 40\n''')
PY

sql="LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT sample_count FROM telemetry_metadata('$fixture') WHERE name='Speed') = 4 THEN true ELSE error('bad channel metadata') END;
SELECT CASE WHEN (SELECT DISTINCT format FROM telemetry_metadata('$fixture')) = 'pds' THEN true ELSE error('format detection failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('chunk order was not preserved') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=1, channels='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('wide scan failed') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_telemetry('$fixture', rate=2, channels='Speed', end_ns=3000000000)) = [10.0, 10.5, 11.0, 11.5, 12.0, 12.5] THEN true ELSE error('mixed-rate interpolation failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', filename=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename option failed') END;
SELECT CASE WHEN (SELECT filename FROM read_telemetry('$fixture', channels='Speed', add_filename_as_column=true) LIMIT 1) = '$fixture' THEN true ELSE error('filename alias failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$motec_fixture', channel='Speed')) = [1.0, 2.0, 3.0, 4.0] THEN true ELSE error('MoTeC parser failed') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM telemetry_samples('$vbo_fixture', channel='velocity kmh')) = [10.0, 20.0, 30.0, 40.0] THEN true ELSE error('VBO parser failed') END;
SELECT CASE WHEN (SELECT list(DISTINCT format ORDER BY format) FROM telemetry_metadata('$fixture_dir/*')) = ['motec', 'pds', 'vbo'] THEN true ELSE error('mixed-format glob failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_cosworth('$fixture', channels='Speed', rate=1)) = 4 THEN true ELSE error('read_cosworth failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_motec('$motec_fixture', channels='Speed', rate=2)) = 4 THEN true ELSE error('read_motec failed') END;
SELECT CASE WHEN (SELECT count(*) FROM read_vbo('$vbo_fixture', channels='velocity kmh', rate=2)) = 4 THEN true ELSE error('read_vbo failed') END;
SELECT CASE WHEN (SELECT typeof(create_date) FROM read_telemetry('$fixture', channels='Speed', add_create_date_column=true, create_date_from=TIMESTAMP '1970-01-01', create_date_to=TIMESTAMP '2100-01-01') LIMIT 1) = 'TIMESTAMP' THEN true ELSE error('create date column failed') END;"
results="$("$DUCKDB" -unsigned -csv -noheader -c "$sql")"
[[ "$(grep -c '^true$' <<<"$results")" = 14 ]]

stats="$(python3 scripts/telemetry_stats.py "$fixture" --extension "$EXTENSION" --duckdb "$DUCKDB" --rate 2 --channels Speed)"
grep -q '^Raw mixed-rate sample stats$' <<<"$stats"
grep -q '^Interpolated wide stats at 2 Hz$' <<<"$stats"
grep -q "$(basename "$fixture")" <<<"$stats"

printf 'integration tests passed\n'
