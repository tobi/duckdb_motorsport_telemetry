---
name: duckdb-motorsport-telemetry
description: Query AiM MP4 files with an `aimd` telemetry track, Pi/Cosworth PDS, MoTeC LD, and Racelogic VBO telemetry directly in DuckDB. Use when inspecting telemetry channels, extracting native samples, resampling mixed-rate channels, comparing files, converting telemetry to Parquet, or pruning telemetry archives by file creation date.
---

# DuckDB Motorsport Telemetry

Use the extension's exact metadata/sample relations before choosing a wide resampling rate. Preserve source units unless the analysis explicitly converts them. Native DuckDB reads local AiM `.mp4` files when they contain an `aimd` telemetry track; the DuckDB-Wasm/browser build does not link the AiM parser.

## Load

Published builds are unsigned. Start DuckDB 1.4.3 with `duckdb -unsigned`, then install over HTTPS:

```sql
INSTALL httpfs;
LOAD httpfs;
INSTALL motorsport_telemetry
FROM 'https://pages.tobi.lutke.com/duckdb_motorsport_telemetry';
LOAD motorsport_telemetry;
```

For a manually downloaded release artifact, use `INSTALL '/absolute/path/motorsport_telemetry.duckdb_extension'` instead.

## Workflow

1. Discover exact names, units, rates, and whether channels actually have samples:

```sql
SELECT format, name, unit, unit_source, canonical_unit, dimension,
       frequency_hz, sample_count
FROM telemetry_metadata('run.pds')
ORDER BY name;
```

A definition with `sample_count = 0` is not recorded data. Always read
`unit_source` alongside `unit`; see Units below, and note PDS values are SI.

2. Use `telemetry_samples` when exact native values or mixed rates matter:

```sql
SELECT time_ns, channel, value, unit
FROM telemetry_samples(
  'run.pds', channel := 'Speed_Ref,I_ACCEL_LONG',
  start_ns := 500000000000, end_ns := 510000000000);
```

3. Use a wide reader only when channels need a common timeline:

```sql
SELECT * FROM read_cosworth(
  'run.pds', rate := 100,
  channels := 'Speed_Ref,I_ACCEL_LONG,gear_pos');
```

Available readers:

- `read_telemetry` — auto-detect `.pds`, `.ld`, `.vbo`, and `.mp4` files; MP4 requires an `aimd` track
- `read_aim` — AiM `aimd` telemetry embedded in MP4
- `read_aimd` — alias for `read_aim`
- `read_cosworth` — Pi/Cosworth PDS
- `read_motec` — MoTeC LD
- `read_vbo` — Racelogic VBOX VBO

## AiM MP4

Use native DuckDB for local MP4 recordings with an `aimd` sample-entry track:

```sql
SELECT name, unit, frequency_hz, sample_count
FROM telemetry_metadata('session.mp4')
WHERE sample_count > 0
ORDER BY name;

SELECT time_ns, channel, value, unit
FROM telemetry_samples(
  'session.mp4',
  channel := 'RPM,GPS Speed,GPS Latitude,GPS Longitude'
);

SELECT *
FROM read_aim(
  'session.mp4',
  rate := 10,
  channels := 'RPM,GPS Speed,GPS Latitude,GPS Longitude'
);
```

The parser reads the MP4 `aimd` track directly and never decodes video or
audio. A valid 56-byte `GPS0` aggregate expands into 15 channels, including
latitude, longitude, altitude, speed, heading, satellite count, position and
speed accuracy, ECEF velocity, GPS time/week, DOP, and fix flags. `LapPk` is
not fabricated when its payload is absent. MP4, `read_aim`, and `read_aimd` are
unavailable in the WASM/browser build because that build does not link the AiM
parser.

## Pushdown rules

Always put physical selections in named arguments:

- `channel` for exact long-form scans
- `channels` for physically restricted wide scans; omit it to expose every sampled channel
- `start_ns`, `end_ns` for session time
- `create_date_from`, `create_date_to` for archive file dates

DuckDB pushes projected columns into the extension, so do not use `SELECT *` when only two channels are needed. DuckDB 1.4 does not pass arbitrary `WHERE` filters to public-C-API table functions; a `WHERE` clause alone does not prevent decoding/opening.

```sql
SELECT filename, max("Speed_Ref")
FROM read_cosworth(
  '**/Offloaded/*.pds',
  channels := 'Speed_Ref',
  filename := true,
  timestamps := true,
  create_date_from := TIMESTAMP '2026-07-01',
  create_date_to := TIMESTAMP '2026-08-01')
GROUP BY filename;
```

## Interpolation

- Default: `interpolate := 'linear'`.
- Only floating-point source channels can interpolate linearly.
- Every integer source channel always uses previous-value semantics.
- Float-backed gear, lap counters/beacons, switches, flags, status, state, and alarms also remain stepwise.
- Use `interpolate := 'previous'` to force step interpolation for all channels.
- Keep integer nanoseconds as the canonical clock.

## Globs

Recursive globs and `{pds,ld,vbo}` are supported:

```sql
SELECT * FROM telemetry_metadata('weekend/**/*.{pds,ld,vbo}');
```

Quote globs in shell commands. Unknown extensions from broad globs are ignored; malformed files with a supported extension fail the bind. Small Pi `Telemetry/` snapshot PDS files are currently unsupported, so prefer `**/Offloaded/*.pds` for that corpus.

## Units

Values and units are source-exact. Units come from the file, never guessed from
channel names. Check `unit_source` before trusting or converting a unit:

| `unit_source` | Meaning |
|---|---|
| `declared` | File stored an explicit unit string |
| `spec_default` | Unit fixed by the format spec |
| `unknown` | No unit info; `unit` is empty. Genuinely unitless (counters, gear, flags, ratios) |

```sql
SELECT name, unit, unit_source FROM telemetry_metadata('run.pds');
```

**Cosworth/Pi PDS stores SI base units.** This surprises people, so check
`unit_source` and the value range before assuming a channel is in display units:

| Quantity | PDS stores | NOT |
|---|---|---|
| Speed | m/s (74.75 = 269 km/h) | km/h |
| Angle, steering, GPS lat/long | rad (-2.58 = -148 deg) | deg |
| Pressure | Pa (7515500 = 75 bar) | bar/kPa |
| Acceleration | m/s^2 (-29.8 = -3.0 g) | g |
| Temperature | K | C |
| Length, damper travel | m (0.0182 = 18.2 mm) | mm |
| Engine speed | rad/s (x9.549 -> RPM) | RPM |

PDS definition records carry a quantity code naming each channel's dimension, so
units resolve even in Pi Toolbox exports that strip the unit string. Field offsets
are detected per file (layouts vary by firmware/Toolbox version), not hardcoded.

Convert with `telemetry_convert(value, from, to)` rather than by hand: it checks
dimensions and errors instead of returning a wrong number.

```sql
SELECT telemetry_convert(max("Speed_Ref"), 'm/s', 'km/h') AS top_kmh,
       telemetry_convert(min("I_ACCEL_LONG"), 'm/s^2', 'g') AS braking_g
FROM read_cosworth('run.pds');
```

Do not silently combine m/s with km/h, Pa with bar, ratio with percent, or m/s^2 with g.

## Unit registry

Every unit is registered: 87 canonical units over 25 dimensions, 243 spellings.
Use it instead of string-matching unit text.

`telemetry_metadata` reports `canonical_unit` and `dimension` next to the file's
own `unit`, which is what makes channels from different systems comparable
(`sec`/`s`, `Lambda`/`ratio`, `cnt`/`count` all normalise).

```sql
SELECT unit AS file_unit, canonical_unit, dimension, count(*)
FROM telemetry_metadata('*.pds') WHERE unit <> '' GROUP BY ALL;

SELECT * FROM telemetry_units() WHERE dimension = 'pressure' AND is_canonical;
```

`telemetry_convert` errors, by design, on:
- different dimensions: `m/s` -> `bar`, `gear` -> `%`
- markers, which have no scale: `flag` -> `raw`
- unknown units: `furlongs`

Use `telemetry_can_convert(from, to)` for a boolean when you need to branch
instead of fail. Markers (`raw`, `flag`, `Driver`) and counts (`gear`, `laps`)
deliberately refuse to scale, so "unitless" is not a hole in the type system.

If a real file has a unit the registry does not know, that is a bug: add it to
`crates/telemetry-core/src/units.rs` and re-run `tests/verify_units.sh`.

## Verifying units against real data

`tests/verify_units.sh FILE_OR_GLOB` checks claims rather than asserting them:
unit coverage by provenance, that every unit string in the data resolves in the
registry, per-dimension SI value envelopes, discrete-channel sanity, and that
every channel's real min/max round-trips through every other unit of its
dimension. Run it after touching unit code or when a new file looks wrong.

It catches real problems: broken channels (`GB_Gearpot_Active` at -2.96e37),
export-side interpolation of discrete channels (Toolbox resampling turned `gear`
into 4.2/4.4/4.6/4.8), and constant channels that are configured but never vary.

## Writing MoTeC LD

```sql
SELECT * FROM write_telemetry('run.pds', 'run.ld');   -- returns channels/samples/bytes
```

Lossless or it errors: f64 stays f64, u16/u32 widen. Refuses (rather than degrading)
mixed sample rates, non-contiguous chunks, non-integer frequencies, or over-long
names/units. Optional: `driver`, `vehicle`, `venue`, `event`, `session`, `comment`,
`date`, `time`.

## Channel maps (advanced, only when asked)

Readers pass data through untouched by default. Use `channel_map` only when the user
explicitly wants renaming/unit conversion (e.g. loading Cosworth SI into a tool
expecting km/h and deg, or unifying schemas across cars). Do not reach for it just
because raw SI values look unfamiliar; convert in SQL instead.

Rules, one per line: `source -> target_name [unit] *scale +offset` (conversion is
`value * scale + offset`; only the source is required).

```sql
SELECT * FROM read_cosworth('run.pds', channel_map := '
    Speed_Ref    -> Ground Speed  [km/h] *3.6
    STEER        -> Steered Angle [deg]  *57.29577951308232
    P_F_BRAKE    -> Brake Press   [bar]  *0.00001
    I_ACCEL_LONG -> G Force Long  [g]    *0.10197162129779283
    ACT          -> Air Temp      [C]    +-273.15
');
SELECT * FROM read_cosworth('run.pds', channel_map := 'maps/oreca07.map');  -- or a file
```

Works on `read_telemetry`/`read_cosworth`/etc, `telemetry_samples`,
`telemetry_metadata`, `telemetry_column_comments`. Mapped units report as `declared`.
Typos and malformed rules error at bind time. Never auto-generate a map from channel
names: in one export `gear` reads 1-6 while `gear_pos` reads 2-7 for the same gear,
and `FIA_Gps*` channels exist but are flat zero.

### Persisting units on a table or in Parquet

DuckDB cannot comment a table function's result columns, so materialise first:

```sql
CREATE TABLE laps AS SELECT * FROM read_cosworth('run.pds', rate := 20);
-- then execute each statement returned by:
SELECT ddl FROM telemetry_column_comments('run.pds', 'laps');
SELECT column_name, comment FROM duckdb_columns() WHERE table_name = 'laps';
```

See `tests/unit_metadata_demo.sh` for the full working loop.

`COPY ... TO parquet` **drops column comments**, so for exports use the
`kv_metadata` column instead. `telemetry_column_comments` returns three carriers:

| Column | Use |
|---|---|
| `ddl` | `COMMENT ON COLUMN` for a materialised table (catalog only) |
| `kv_metadata` | Payload for Parquet `KV_METADATA` (survives export) |
| `channel_map_rule` | The `channel_map` rule reproducing this column's unit |

```sql
COPY laps TO 'laps.parquet' (FORMAT PARQUET, KV_METADATA {
  'Alt_RPM': 'unit=rad/s; source=declared; dimension=angular_velocity; base_unit=rad/s'
});
SELECT key::VARCHAR, value::VARCHAR FROM parquet_kv_metadata('laps.parquet');
```

Use `channel_map_rule` to derive a map from the file's own units instead of
hand-writing one. `tests/units_metadata_e2e.sh` proves both carriers end to end.

## Common queries

```sql
-- Native logging inventory
SELECT name, unit, list(DISTINCT frequency_hz ORDER BY frequency_hz)
FROM telemetry_metadata('**/*.ld')
WHERE sample_count > 0
GROUP BY name, unit;

-- Direct Parquet conversion
COPY (
  SELECT * FROM read_motec(
    '**/*.ld', rate := 100,
    channels := 'Corr Speed,P_F_BRAKE,Gear', filename := true)
) TO 'telemetry.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

-- Check whether lateral acceleration was truly logged
SELECT file, name, unit, frequency_hz, sample_count
FROM telemetry_metadata('**/*.{pds,ld,vbo}')
WHERE lower(name) LIKE '%accel%lat%' OR lower(name) IN ('glat', 'lateral g');
```

For an overview, run `scripts/telemetry_stats.py PATH_OR_GLOB`.
