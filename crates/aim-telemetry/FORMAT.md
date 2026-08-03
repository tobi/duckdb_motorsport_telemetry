# AiM `aimd` MP4 notes

This documents the portions of the format used by the reader. AiM does not appear to publish the embedded-track binary specification publicly; field names below distinguish observed framing from inferred meaning.

## MP4 layer

The telemetry track uses:

- handler type `meta` (handler name observed as `MetaAimHandler`)
- sample-entry FourCC `aimd`
- a millisecond MP4 timescale in the examined recording
- approximately one MP4 sample per 100 ms

The reader identifies the track from `stsd`'s `aimd` sample entry. It supports normal and extended-size MP4 boxes, `stco` and `co64`, constant or per-sample `stsz`, multi-entry `stsc`, and run-length `stts` timing. It does not assume a track index or contiguous data track.

## AiM packet envelope

Every observed MP4 sample begins with:

| Offset | Encoding | Meaning |
|---:|---|---|
| 0 | `u16be` | bytes following this field |
| 2 | byte | observed `0x40` |
| 3 | 3 bytes | reserved/zero in the examined file |
| 6 | ASCII | `amv0` signature |

The first packet additionally contains the observed stream-version suffix `s1` and a configuration/schema section. Data packets carry timestamped scalar records and tagged aggregate records.

## Tagged schema blocks

Blocks use this observed header:

```text
'<' 'h' TAG[3] 0x00 payload_length:u32le 0x01 '>' payload...
```

The parser searches the first packet for every `CHS` block rather than assuming count, order, or byte position. Scalar channel definitions currently use these fields:

| CHS payload offset | Encoding | Use |
|---:|---|---|
| 0 | `u32le` | record ID used by data packets |
| 6 | `u16le` | observed source/logger channel ID (currently not exposed) |
| 20 | `u32le` | scalar encoding kind (`12` is the observed integer timer encoding) |
| 24 | 8-byte C string | short code |
| 32 | 32-byte C string | displayed channel name |
| 68 | `u32le` | observed aggregate-buffer offset; not used for scalar decoding |
| 72 | `u32le` | encoded value width |
| 80 | `u32le` | schema class (`0x1003` is the observed unsigned protocol clock) |

Unknown fields are not treated as calibration or units. This avoids reporting guessed metadata as declared metadata.

## Scalar sample records

Scalar updates are self-delimiting:

```text
'(' 'S' timestamp_ms:u32le record_id:u16le value[declared_width] ')'
```

The record ID is joined to the `CHS` schema. Timestamps are normalized to the first scalar timestamp. Each channel's native period is the modal positive timestamp delta; a gap greater than twice that period starts a new telemetry chunk.

Widths of one and four bytes are currently exposed. One-byte records are unsigned status values. Four-byte ordinary channels use IEEE-754 little-endian values. Schema class `0x1003` selects an unsigned protocol clock, while encoding kind `12` selects signed integer timer ticks. These choices come from `CHS` fields rather than channel names.

## Aggregate records

`LapPk` (20 bytes) and `GPS0` (56 bytes in the examined recording) are tagged aggregate structures rather than scalar `(S … )` records. They are deliberately omitted until their nested fields can be identified from schema data or an authoritative specification. The reader does not manufacture scalar GPS channels from fixed offsets.

## Compatibility policy

- Read ISO-BMFF and AiM lengths, IDs, names, widths, and timestamps from the file.
- Do not depend on the observed 40-channel order or on MP4 track 3.
- Accept additional unknown tagged blocks.
- Reject malformed sample tables, out-of-file offsets, missing `amv0` packet signatures, duplicate record IDs, and MP4 files without an `aimd` sample entry.
- Leave units unknown until they can be decoded reliably from the schema.
