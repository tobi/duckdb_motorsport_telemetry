#!/usr/bin/env python3
"""Generate small deterministic fixtures for every supported telemetry format."""
from __future__ import annotations

import struct
import sys
from pathlib import Path

SAMPLES = {
    "speed": (10.0, 11.0, 12.0, 13.0),
    "throttle": (0.0, 50.0, 100.0, 25.0),
    "brake": (0.0, 10.0, 40.0, 5.0),
    "g_lat": (0.0, 0.2, -0.3, 0.1),
    "g_long": (0.0, -0.5, 0.1, 0.2),
    "distance": (0.0, 10.0, 20.0, 30.0),
    "lap": (1.0, 1.0, 1.0, 1.0),
    "latitude": (43.7978, 43.7979, 43.7980, 43.7981),
    "longitude": (-87.9899, -87.9898, -87.9897, -87.9896),
}


def mp4_box(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">I4s", len(payload) + 8, kind) + payload


def channel_definition(record_id: int, name: str, width: int) -> bytes:
    payload = bytearray(112)
    struct.pack_into("<I", payload, 0, record_id)
    encoded = name.encode("ascii")
    payload[24 : 24 + len(encoded)] = encoded
    payload[32 : 32 + len(encoded)] = encoded
    struct.pack_into("<I", payload, 72, width)
    return bytes(payload)


def packet_header(signature: bytes) -> bytearray:
    return bytearray(b"\x00\x00\x40\x00\x00\x00" + signature)


def make_aimd() -> bytes:
    schema = packet_header(b"amv0s1")
    for record_id, name, width in ((42, "RPM", 4), (55, "GPS0", 56)):
        definition = channel_definition(record_id, name, width)
        schema.extend(b"<hCHS\x00")
        schema.extend(struct.pack("<I", len(definition)))
        schema.extend(b"\x01>")
        schema.extend(definition)
    struct.pack_into(">H", schema, 0, len(schema) - 2)

    values = packet_header(b"amv0")
    values.extend(b"(S")
    values.extend(struct.pack("<I", 100))
    values.extend(struct.pack("<H", 42))
    values.extend(struct.pack("<f", 1234.5))
    values.extend(b")")
    gps = bytearray(56)
    struct.pack_into("<I", gps, 0, 100)
    struct.pack_into("<I", gps, 4, 573_634_560)
    struct.pack_into("<H", gps, 12, 2429)
    struct.pack_into("<3i", gps, 16, 16_174_352, -460_842_617, 439_210_627)
    struct.pack_into("<I", gps, 28, 783)
    struct.pack_into("<3i", gps, 32, 5, -10, 8)
    struct.pack_into("<I", gps, 44, 6)
    struct.pack_into("<I", gps, 48, 0x0900_00F8)
    struct.pack_into("<I", gps, 52, 4096)
    values.extend(b"<hGPS\x00")
    values.extend(struct.pack("<I", len(gps)))
    values.extend(b"\x01>")
    values.extend(gps)
    struct.pack_into(">H", values, 0, len(values) - 2)

    ftyp = mp4_box(b"ftyp", b"isom\x00\x00\x00\x00isom")
    mdat = mp4_box(b"mdat", bytes(schema) + bytes(values))
    chunk_offset = len(ftyp) + 8
    mdhd = bytearray(24)
    struct.pack_into(">I", mdhd, 12, 1000)
    hdlr = bytearray(24)
    hdlr[8:12] = b"meta"
    stsd = bytearray(16)
    stsd[7] = 1
    struct.pack_into(">I", stsd, 8, 8)
    stsd[12:16] = b"aimd"
    stts = bytearray(16)
    stts[7] = 1
    struct.pack_into(">II", stts, 8, 2, 100)
    stsc = bytearray(20)
    stsc[7] = 1
    struct.pack_into(">III", stsc, 8, 1, 2, 1)
    stsz = bytearray(20)
    struct.pack_into(">II", stsz, 8, 2, len(schema))
    struct.pack_into(">I", stsz, 16, len(values))
    stco = bytearray(12)
    stco[7] = 1
    struct.pack_into(">I", stco, 8, chunk_offset)
    stbl = mp4_box(
        b"stbl",
        b"".join(
            mp4_box(kind, payload)
            for kind, payload in (
                (b"stsd", stsd),
                (b"stts", stts),
                (b"stsc", stsc),
                (b"stsz", stsz),
                (b"stco", stco),
            )
        ),
    )
    minf = mp4_box(b"minf", stbl)
    mdia = mp4_box(b"mdia", mp4_box(b"mdhd", mdhd) + mp4_box(b"hdlr", hdlr) + minf)
    moov = mp4_box(b"moov", mp4_box(b"trak", mdia))
    return ftyp + mdat + moov


def make_pds() -> bytes:
    channels = [
        (1, "Speed", "m/s", SAMPLES["speed"]),
        (2, "Throttle Pos", "%", SAMPLES["throttle"]),
        (3, "Brake Pedal Pos", "%", SAMPLES["brake"]),
        (4, "G_FORCE_LAT", "m/s^2", SAMPLES["g_lat"]),
        (5, "G_FORCE_LONG", "m/s^2", SAMPLES["g_long"]),
        (6, "Lap Distance", "m", SAMPLES["distance"]),
        (7, "Lap Number", "count", SAMPLES["lap"]),
        (8, "GPS Latitude", "deg", SAMPLES["latitude"]),
        (9, "GPS Longitude", "deg", SAMPLES["longitude"]),
    ]
    definition_width = 0xC0
    defs = 0x200
    chunks = defs + definition_width * len(channels)
    chunk_width = 0x40
    chunk_count = len(channels) * 2
    end = chunks + chunk_width * chunk_count
    data = bytearray(0x1800)
    data[0x40:0x80] = b"\xff" * 0x40

    def u32(offset: int, value: int) -> None:
        struct.pack_into("<I", data, offset, value)

    def directory(offset: int, section: int, count: int, class_a: int, class_b: int, next_count: int) -> None:
        u32(offset, section)
        u32(offset + 8, count)
        u32(offset + 0x10, class_a)
        u32(offset + 0x14, class_b)
        u32(offset + 0x18, next_count)

    def definition(offset: int, channel_id: int, name: str, unit: str) -> None:
        u32(offset, channel_id)
        name_bytes = name.encode("utf-16le")
        unit_bytes = unit.encode("utf-16le")
        data[offset + 8 : offset + 8 + len(name_bytes)] = name_bytes
        data[offset + 0x90 : offset + 0x90 + len(unit_bytes)] = unit_bytes

    def chunk(offset: int, order: int, channel_id: int, pointer: int) -> None:
        u32(offset, order)
        u32(offset + 4, channel_id)
        u32(offset + 8, channel_id)
        u32(offset + 0x18, 10_000_000)
        u32(offset + 0x1C, 2)
        u32(offset + 0x38, pointer)

    directory(0x80, defs, len(channels), 8, 1, chunk_count)
    directory(0xA0, chunks, chunk_count, 1, 3, 0)
    directory(0xC0, end, 0, 1, 1, 0)
    for index, (channel_id, name, unit, values) in enumerate(channels):
        definition(defs + index * definition_width, channel_id, name, unit)
        for pair in range(2):
            chunk_index = index * 2 + pair
            pointer = 0x1000 + chunk_index * 0x20
            chunk(chunks + chunk_index * chunk_width, chunk_index + 1, channel_id, pointer)
            struct.pack_into("<2d", data, pointer, *values[pair * 2 : pair * 2 + 2])
    return bytes(data)


def make_motec() -> bytes:
    channels = [
        ("Speed", "m/s", SAMPLES["speed"]),
        ("Throttle Pos", "%", SAMPLES["throttle"]),
        ("Brake Pedal Pos", "%", SAMPLES["brake"]),
        ("G_FORCE_LAT", "m/s^2", SAMPLES["g_lat"]),
        ("G_FORCE_LONG", "m/s^2", SAMPLES["g_long"]),
        ("Lap Distance", "m", SAMPLES["distance"]),
        ("Lap Number", "count", SAMPLES["lap"]),
        ("GPS Latitude", "deg", SAMPLES["latitude"]),
        ("GPS Longitude", "deg", SAMPLES["longitude"]),
    ]
    block_size = 124
    first_block = 0x200
    data_start = first_block + block_size * len(channels) + 0x40
    data = bytearray(data_start + 4 * len(channels) * 4)
    struct.pack_into("<I", data, 0, 0x40)
    struct.pack_into("<I", data, 0x08, first_block)
    for index, (name, unit, values) in enumerate(channels):
        block = first_block + index * block_size
        next_block = block + block_size if index + 1 < len(channels) else 0
        pointer = data_start + index * 4 * 4
        struct.pack_into("<I", data, block + 4, next_block)
        struct.pack_into("<I", data, block + 8, pointer)
        struct.pack_into("<I", data, block + 0x0C, 4)
        struct.pack_into("<H", data, block + 0x12, 0x07)
        struct.pack_into("<H", data, block + 0x14, 4)
        struct.pack_into("<H", data, block + 0x16, 2)
        encoded_name = name.encode("ascii")
        encoded_unit = unit.encode("ascii")
        data[block + 0x20 : block + 0x20 + len(encoded_name)] = encoded_name
        data[block + 0x48 : block + 0x48 + len(encoded_unit)] = encoded_unit
        struct.pack_into("<4f", data, pointer, *values)
    return bytes(data)


def make_vbo() -> bytes:
    columns = [
        "sats", "time", "lat", "long", "velocity", "heading", "height",
        "vert-vel", "Tsample", "solution_type", "avifileindex", "avitime",
        "throttle", "brake", "gforce_lat", "gforce_long", "distance", "lap",
    ]
    rows = []
    for index in range(4):
        rows.append([
            9, 120000.0 + index * 0.5, SAMPLES["latitude"][index],
            SAMPLES["longitude"][index], 10.0 + index * 10.0, 90,
            291.74, 0, 0.5, 3, 0, 0, SAMPLES["throttle"][index],
            SAMPLES["brake"][index], SAMPLES["g_lat"][index],
            SAMPLES["g_long"][index], SAMPLES["distance"][index],
            SAMPLES["lap"][index],
        ])
    units = ["%", "%", "m/s^2", "m/s^2", "m", "count"]
    return (("[column names]\n" + " ".join(columns) + "\n[channel units]\n")
            + "\n".join(units) + "\n[data]\n"
            + "\n".join(" ".join(str(value) for value in row) for row in rows)
            + "\n").encode()


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} OUTPUT_DIRECTORY")
    output = Path(sys.argv[1])
    output.mkdir(parents=True, exist_ok=True)
    files = {
        "synthetic_aimd.mp4": make_aimd(),
        "synthetic_cosworth.pds": make_pds(),
        "synthetic_motec.ld": make_motec(),
        "synthetic_vbo.vbo": make_vbo(),
    }
    for name, content in files.items():
        (output / name).write_bytes(content)
        print(f"{output / name}: {len(content)} bytes")


if __name__ == "__main__":
    main()
