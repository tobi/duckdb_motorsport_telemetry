#!/usr/bin/env bash
set -euo pipefail

: "${DUCKDB:=duckdb}"
: "${EXTENSION:?EXTENSION must point to pds.duckdb_extension}"
fixture="$(mktemp --suffix=.pds)"
trap 'rm -f "$fixture"' EXIT

python3 - "$fixture" <<'PY'
import struct, sys
p = sys.argv[1]
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
PY

sql="LOAD '$EXTENSION';
SELECT CASE WHEN (SELECT sample_count FROM pds_channels('$fixture') WHERE name='Speed') = 4 THEN true ELSE error('bad channel metadata') END;
SELECT CASE WHEN (SELECT list(value ORDER BY sample_index) FROM pds_samples('$fixture', channel='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('chunk order was not preserved') END;
SELECT CASE WHEN (SELECT list(\"Speed\" ORDER BY time_ns) FROM read_pds('$fixture', rate=1, channels='Speed')) = [10.0, 11.0, 12.0, 13.0] THEN true ELSE error('wide scan failed') END;"
results="$("$DUCKDB" -unsigned -csv -noheader -c "$sql")"
[[ "$(grep -c '^true$' <<<"$results")" = 3 ]]
printf 'integration tests passed\n'
