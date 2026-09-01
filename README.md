# DuckDB Motorsport Telemetry

**[Open the browser telemetry lab →](https://pages.tobi.lutke.com/duckdb_motorsport_telemetry/)**

A fast, vectorized DuckDB extension and reusable Rust parser workspace for:

- AiM Sports embedded **aimd** telemetry (`.mp4`)
- Pi Research / Cosworth **PDS** (`.pds`)
- MoTeC i2 **LD** (`.ld`)
- Racelogic VBOX **VBO** (`.vbo`)
- **`.telemetry`** recordings written by [motorsport-telemetry-rs](https://github.com/tobi/motorsport-telemetry-rs) — a zstd-compressed MTJ JSONL document, or the legacy zip container, told apart by content; plus `.telemetry.jsonl[.zstd]`

The exact model is two relations—channel metadata and native-rate samples—plus a friendly interpolated wide reader. Files are memory-mapped where possible, values decode directly into DuckDB vectors, scans are parallel, and projection pushdown avoids decoding channels a query does not use.

Input support is split by runtime:

| Input | Native DuckDB | DuckDB-Wasm/browser |
|---|---:|---:|
| AiM MP4 with an `aimd` track | Yes | No |
| Pi/Cosworth PDS | Yes | Yes |
| MoTeC LD | Yes | Yes |
| Racelogic VBO | Yes | Yes |
| `.telemetry` / MTJ | Yes | No |

The MP4 reader is a native DuckDB feature in v0.7.0. The DuckDB-Wasm build does not link the AiM parser; `.mp4` inputs and the `read_aim`/`read_aimd` functions are unavailable in the browser.

## Easiest installation

Install the signed DuckDB Community Extension:

```sql
INSTALL motorsport_telemetry FROM community;
LOAD motorsport_telemetry;
```

Verify it against a telemetry file:

```sql
SELECT name, unit, frequency_hz, sample_count
FROM telemetry_metadata('/path/to/run.pds');
```

DuckDB downloads the platform-specific build from the Community Extension endpoint and verifies its signature. `httpfs` and `-unsigned` are not required. Subsequent sessions only need:

```sql
LOAD motorsport_telemetry;
```

If DuckDB still finds an older cached `0.6.0` artifact, refresh it with:

```sql
FORCE INSTALL motorsport_telemetry FROM community;
```

The checked-in release version is maintained in [`VERSION`](VERSION), and all package, artifact, and release metadata is synchronized from that file.

## Browser telemetry lab

Open **[Telemetry Lab](https://pages.tobi.lutke.com/duckdb_motorsport_telemetry/)** to analyze a PDS, LD, or VBO recording without installing anything. Drop a `.pds`, `.ld`, or `.vbo` file into the page; the file stays in your browser and is never uploaded. The DuckDB-Wasm build does not link the AiM parser, so AiM `.mp4` inputs and the `read_aim`/`read_aimd` functions are unavailable there.

The lab runs this same Rust extension as a DuckDB-Wasm side module and automatically shows:

- recorded versus empty channel definitions
- exact native rates, sample counts, units, and duration
- likely speed, brake, throttle, acceleration, and gear signals
- unit-normalized top speed, directional G, combined G, and best-lap headlines
- lap detection from counters, timer/distance resets, and VBOX start-line crossings
- a per-lap quick trace that defaults to the best complete lap
- distance-based solid/dashed lap overlays with scrubbed time delta
- synchronized trace and GPS-map scrubbing with speed, throttle, and brake at the cursor
- click-through channel inspection with exact samples from the selected lap
- persistent speed, pedal, G, and distance role overrides for unusual logger naming
- a checksum-verified, browser-cached Lamborghini GT3 Barcelona demo
- a full SQL workbench plus ten adaptive query recipes

The browser smoke test generates synthetic PDS, MoTeC, and VBO files at runtime, drops each into Chromium, verifies parsing, and executes SQL. No real telemetry fixture is committed.

## SQL in 30 seconds

```sql
LOAD motorsport_telemetry;

-- Discover channels and native rates. MP4 is accepted when it contains aimd.
SELECT format, name, unit, frequency_hz, sample_count
FROM telemetry_metadata('run.pds')
WHERE sample_count > 0
ORDER BY name;

-- Exact, native-rate samples. No interpolation.
SELECT time_ns / 1e9 AS seconds, value
FROM telemetry_samples('run.pds', channel := 'Speed_Ref')
ORDER BY time_ns;

-- Friendly 100 Hz, positionally aligned wide table.
SELECT time_ns, "Speed_Ref", "I_ACCEL_LONG", "gear_pos"
FROM read_telemetry(
    'run.pds',
    rate := 100,
    channels := 'Speed_Ref,I_ACCEL_LONG,gear_pos'
);
```

### AiM video telemetry

AiM SmartyCam-style MP4 recordings with an `aimd` sample-entry track are queried directly by native DuckDB; no FFmpeg extraction step is required. The file must be a local path because remote/httpfs paths are not supported.

Start with metadata. Channel names come from the recording's `CHS` schema, so inspect them before choosing a projection:

```sql
SELECT name, unit, data_type, frequency_hz, sample_count
FROM telemetry_metadata('session.mp4')
WHERE sample_count > 0
ORDER BY name;
```

Read exact native-rate samples, including derived GPS channels:

```sql
SELECT time_ns / 1e9 AS seconds, channel, value, unit
FROM telemetry_samples(
    'session.mp4',
    channel := 'RPM,GPS Speed,GPS Latitude,GPS Longitude'
)
ORDER BY channel, time_ns;
```

Use either the generic reader or the AiM-specific aliases for a synchronized wide result:

```sql
SELECT time_ns, "RPM", "GPS Speed", "GPS Latitude", "GPS Longitude"
FROM read_aim(
    'session.mp4',
    channels := 'RPM,GPS Speed,GPS Latitude,GPS Longitude',
    rate := 10
);

-- `read_aimd` is an alias; `read_telemetry` auto-detects `.mp4`.
```

### GPS0 output

Each supported 56-byte `GPS0` aggregate produces these 15 channels:

`GPS Latitude`, `GPS Longitude`, `GPS Altitude`, `GPS Speed`, `GPS Heading`,
`GPS Satellites`, `GPS Position Accuracy`, `GPS Speed Accuracy`,
`GPS ECEF Velocity X`, `GPS ECEF Velocity Y`, `GPS ECEF Velocity Z`, `GPS iTOW`,
`GPS Week`, `GPS DOP`, and `GPS Fix Flags`.

Position is converted from ECEF centimetres to WGS84 latitude/longitude degrees
and ellipsoid altitude metres. Velocity is converted from ECEF centimetres per
second to metres per second; heading is true heading in degrees. GPS accuracy,
satellite count, DOP, timing, and fix flags retain their documented source
scales. Real GPS09c recordings observed by this reader provide GPS at 25 Hz;
the reader reports the native rate found in each file.

### AiM compatibility boundaries

- The reader identifies `aimd` by the MP4 `stsd` sample-entry FourCC, not by track number or localized handler text.
- It supports normal and extended-size boxes, `stco`/`co64`, constant or per-sample `stsz`, multi-entry `stsc`, and run-length `stts`.
- Scalar records currently expose one-byte status values and four-byte float/integer values from `CHS` definitions.
- `GPS0` is decoded only when its declared payload width is 56 bytes.
- `LapPk` is recognized as a definition but is not fabricated into channels when its payload is absent; lap number and timing remain ordinary scalar channels.
- Unknown scalar units remain `unit_source = unknown` rather than being inferred from channel names.

## Functions

### `telemetry_metadata(path)`

One row per channel definition:

| Column | Meaning |
|---|---|
| `file` | Full input path |
| `format` | `aimd`, `pds`, `motec`, or `vbo` |
| `channel_id` | File-local channel identifier |
| `name`, `unit` | Original channel metadata |
| `unit_source` | Where `unit` came from: `declared`, `spec_default`, or `unknown` |
| `canonical_unit` | Normalised unit spelling, or `NULL` if unrecognised |
| `dimension` | Physical dimension of the unit, or `NULL` if unrecognised |
| `type_code`, `data_type` | Stored representation |
| `frequency_hz`, `sample_period_ns` | Native clock |
| `sample_count`, `chunk_count`, `duration_ns` | Physical storage summary |

```sql
SELECT file, format, count(*) AS definitions,
       count(*) FILTER (sample_count > 0) AS sampled_channels,
       sum(sample_count) AS raw_samples
FROM telemetry_metadata('weekend/**/*.{pds,ld,vbo,mp4}')
GROUP BY file, format;
```

Find lateral acceleration availability:

```sql
SELECT file, name, unit, frequency_hz, sample_count
FROM telemetry_metadata('**/*.{pds,ld,vbo,mp4}')
WHERE lower(name) SIMILAR TO '%(lat|lateral)%(accel|g)%'
ORDER BY file, name;
```

### `telemetry_file_metadata(path)`

Returns one summary row per file without materializing a wide telemetry table.
The native parser uses its memory-mapped index and decodes only semantic
channels needed for the summary:

- internal driver IDs and driver transitions
- embedded driver/vehicle/venue/event/session/date/time identity when available
- absolute clock range and an internal session candidate key when available
- channel/sample counts and a stable schema hash
- lap count, **flying** lap count, stint count, and the fastest flying lap (number, time, label)
- native MP4 video-frame count
- `source_format`: the vendor format a `.telemetry` was converted from

```sql
SELECT file, format, source_format, session_key, driver_ids, driver, vehicle,
       venue, event, session, date, time,
       lap_count, flying_lap_count, stint_count,
       fastest_lap_number, fastest_lap_label, fastest_lap_time_ns,
       video_frame_count
FROM telemetry_file_metadata('race/**/*.{pds,ld,vbo,mp4,telemetry}')
ORDER BY absolute_start_ns;
```

The fastest lap is the shortest plausible **flying** lap (see
`telemetry_laps`): an out-lap, an in-lap cut short by a pit-box counter
reset, or a lap containing a pit stop is never a candidate, however short a
broken beacon made it.

The lap model, the video-sync calls and the "blessed" normalized channels
(speed m/s, pedals 0–1, brake pressure bar, steering deg, rpm, lap number /
stint / label) are specified once, in the
[motorsport-telemetry-rs client contract](https://github.com/tobi/motorsport-telemetry-rs#the-client-contract-blessed-channels-laps-video-clocks).
This extension exposes the source-exact channels (`read_telemetry`), the lap
model (`telemetry_laps`) and the blessed channels (`read_telemetry_normalized`).

### `read_telemetry_normalized(path, ...)`

The SQL mirror of the library's blessed channels: one row per instant at
`rate` Hz with fixed units, resolved from each file's channel names, declared
units and — for unitless dash channels — value ranges, exactly as
[`NormalizedSample`](https://github.com/tobi/motorsport-telemetry-rs#blessed-channels-normalizedsample)
does. Works on every supported format and on `.telemetry`; a column is NULL
where the source has no trustworthy signal for it.

| Column | Unit |
|---|---|
| `time_ns` | file-relative ns |
| `speed_mps` | m/s |
| `throttle_fraction`, `brake_fraction`, `clutch_fraction` | 0–1 (pedal position) |
| `brake_pressure_bar` | bar (line pressure, when logged) |
| `steering_deg` | degrees, sign as logged |
| `gear` | as logged |
| `rpm` | rpm |
| `lap_number` | virtual session lap (1-based, monotonic across stints) |
| `stint`, `stint_lap_number`, `lap_kind`, `lap_label` | the lap model of `telemetry_laps` at this instant |
| `lap_progress` | 0–1 within the lap |
| `lap_time_s` | s since the lap started |
| `latitude_deg`, `longitude_deg` | WGS84 degrees |
| `time_of_day_ns`, `absolute_time_ns` | source clock |

Named arguments: `rate` (1–5000, default 100), `start_ns`, `end_ns`,
`filename`, `create_date_from`, `create_date_to`. Not available in the Wasm
build.

```sql
SELECT lap_label,
       round(max(speed_mps) * 3.6, 1)          AS vmax_kmh,
       round(avg(throttle_fraction) * 100, 1)  AS throttle_pct,
       round(max(brake_pressure_bar), 1)       AS brake_bar,
       max(rpm)                                AS rpm_max
FROM read_telemetry_normalized('run.pds', rate := 20)
WHERE lap_kind = 'flying'
GROUP BY lap_label, lap_number
ORDER BY lap_number;
```

### `telemetry_laps(path)`

One row per lap with the stint-aware lap model resolved. Vendor lap counters
are *stint* counters — an AiM dash resets `Lap_Number` to 0 in the pit box,
a Cosworth logger keeps counting through a stop, a power-cycled logger starts
again at 1 — so the raw counter cannot identify a lap across a session.

| Column | Meaning |
|---|---|
| `lap_number` | Virtual session lap: 1-based, monotonic across the whole recording and every stint |
| `stint` | 1-based stint index (new stint at a counter reset, a >10 s recording gap, after an in-lap, after a pit lap) |
| `stint_lap` | The counter as the dash showed it (0 for an AiM out-lap) |
| `kind` | `flying`, `out`, `in`, `out-in` (stint with no beacon), `pit` (beacon to beacon with ≥15 s standing still inside) |
| `label` | `S1 out`, `S1 L2`, `S2 L3`, `S1 pit L5` — stint-local numbers match the dash display |
| `flying`, `complete` | flying = beacon to beacon and moving throughout; complete = both boundaries inside the recording |
| `start_ns`, `end_ns`, `duration_ns` | File-relative nanoseconds |
| `first_video_frame` | Presentation-order frame at `start_ns` (AiM MP4 / `.telemetry` with video linkage) |

```sql
-- Flying laps per stint, with the samples of the fastest one
WITH laps AS (SELECT * FROM telemetry_laps('run.pds'))
SELECT stint, count(*) FILTER (flying) AS flying, min(duration_ns) FILTER (flying) / 1e9 AS best_s
FROM laps GROUP BY stint ORDER BY stint;

SELECT s.*
FROM read_telemetry('run.pds', rate := 100, channels := 'Speed_Ref,P_F_BRAKE') s
JOIN (SELECT start_ns, end_ns FROM telemetry_laps('run.pds') WHERE label = 'S2 L3') l
  ON s.time_ns >= l.start_ns AND s.time_ns < l.end_ns;
```

### `telemetry_samples(path, ...)`

Exact long-form samples. Channels retain their own frequencies and clocks.

```sql
SELECT file, format, channel, unit, frequency_hz,
       sample_index, time_ns, value
FROM telemetry_samples(
    '**/Offloaded/*.pds',
    channel := 'Speed_Ref,I_ACCEL_LONG',
    start_ns := 500000000000,
    end_ns := 530000000000
);
```

Named arguments:

- `channel := 'name,other name'` physically selects channels
- `start_ns := ...` inclusive scan bound
- `end_ns := ...` exclusive scan bound

Use these arguments instead of relying only on `WHERE`: DuckDB 1.4's public C table-function API supports projection pushdown but not arbitrary SQL filter pushdown.

Common raw statistics:

```sql
SELECT file, channel, any_value(unit) AS unit,
       count(*) AS samples, min(value), avg(value), max(value)
FROM telemetry_samples('race/**/*.{pds,ld,vbo}',
                       channel := 'Speed_Ref,P_F_BRAKE')
GROUP BY file, channel
ORDER BY file, channel;
```

Crash-window extraction without decoding the rest of the session:

```sql
COPY (
    SELECT time_ns / 1e9 AS seconds, channel, value, unit
    FROM telemetry_samples('run.pds',
         channel := 'Speed_Ref,I_ACCEL_LONG',
         start_ns := 519000000000,
         end_ns := 525000000000)
    ORDER BY time_ns, channel
) TO 'impact-window.parquet' (FORMAT PARQUET);
```

### `read_telemetry(path, ...)`

Builds a shared integer-nanosecond timeline and returns original channel names as columns. Format-specific entry points enforce the expected extension while sharing the same arguments and output model:

```sql
SELECT * FROM read_cosworth('run.pds', channels := 'Speed_Ref');
SELECT * FROM read_motec('run.ld', channels := 'Corr Speed');
SELECT * FROM read_vbo('run.vbo', channels := 'velocity kmh');
```

```sql
SELECT *
FROM read_telemetry(
    'run.ld',
    rate := 50,
    channels := 'Corr Speed,Driver Throttle Pos,P_F_BRAKE,Gear',
    interpolate := 'linear',
    filename := true
);
```

Named arguments:

- `rate := 100` — output frequency, 1–5,000 Hz
- `channels := 'A,B,C'` — optional schema and physical channel selection; omit it to expose every sampled channel
- `interpolate := 'linear'` or `'previous'`
- `start_ns`, `end_ns` — physical time-range pruning
- `filename := true` — add a `filename` column
- `add_filename_as_column := true` — explicit alias for `filename`
- `timestamps := true` — add both `create_date` and `modified_at`
- `unit_tags := true` — expose known channel units as strict DuckDB logical type aliases for `telemetry_convert_column`
- `add_create_date_as_column := true` — add filesystem creation time, with modified time as its fallback
- `add_modified_at_as_column := true` — add filesystem modification time
- `create_date_from := TIMESTAMP '2026-07-01'` — inclusive pre-open file pruning
- `create_date_to := TIMESTAMP '2026-08-01'` — exclusive pre-open file pruning

Only floating-point source channels can be linearly interpolated. Every integer source channel always uses previous-value semantics, even when `interpolate := 'linear'`; known float-backed discrete/event channels—gear, lap number/beacon, switches, status, state, flags, alarms, GPS solution type—also remain stepwise. Use `interpolate := 'previous'` to make floating-point channels stepwise too.

Across multiple files, schemas union by case-insensitive channel name. Missing channels are `NULL`.

### `telemetry_session_metadata(path, ...)`

Groups file summaries by internal clock continuity and returns session-wide file
count, driver IDs/stints, lap count, and fastest complete lap:

```sql
SELECT session_key, file_count, driver_ids, stint_count, lap_count,
       fastest_lap_number, fastest_lap_time_ns
FROM telemetry_session_metadata(
    'race/**/*.mp4',
    max_gap_seconds := 60
);
```

### `read_telemetry_session(path, ...)`

Stitches internally continuous files onto one event clock. It does not use
filenames. Session grouping uses:

- AiM: GPS week + GPS `iTOW` + channel-schema signature
- MoTeC: embedded date/time + vehicle + venue + schema signature
- VBO: recorded time-of-day + schema signature

Files are ordered by the internal clock and joined only when the gap is within
`max_gap_seconds` (default 60). If a glob contains more than one session, the
function errors instead of silently combining them.

```sql
SELECT time_ns, source_file, file_time_ns,
       video_file_index, video_frame_index, video_sync_time,
       driver_id, lap_number, "GPS Speed"
FROM read_telemetry_session(
    'race/**/*.mp4',
    rate := 20,
    channels := 'GPS Speed',
    max_gap_seconds := 60
)
ORDER BY time_ns;
```

For this function, `start_ns` and `end_ns` are session-relative bounds and are
translated into each source file's local clock before decoding.

`time_ns` is the continuous event time and preserves real gaps between files;
`file_time_ns` is local to the source recording. For AiM MP4, `source_file`
plus `video_frame_index` identifies the exact video frame. There is no AiM
telemetry channel carrying the frame number: it is derived from the MP4 video
track's `mdhd`/`stts` timing and optional `ctts` composition offsets. VBO
exposes its native `avifileindex`/`avisynctime` as
`video_file_index`/`video_sync_time`; no frame number is invented without an
internally available frame timeline.

`driver_id` and `lap_number` come from internal telemetry channels, not the
filename. The session relation therefore remains valid when recordings are
renamed.

Creation-date pruning happens before telemetry files are opened:

```sql
SELECT filename, create_date, modified_at, max("Speed_Ref")
FROM read_cosworth(
    'archive/**/*.pds',
    channels := 'Speed_Ref',
    filename := true,
    timestamps := true,
    create_date_from := TIMESTAMP '2026-07-01',
    create_date_to := TIMESTAMP '2026-08-01')
GROUP BY filename, create_date, modified_at;
```

A plain `WHERE create_date ...` remains logically correct but is applied after scanning because DuckDB 1.4's public C table-function API does not expose arbitrary filter expressions to extensions. Use `create_date_from`/`create_date_to` when physical pushdown matters.

## Units

Channels report the units the file actually specifies. Nothing is guessed from channel names, and `unit_source` tells you where each unit came from:

| `unit_source` | Meaning |
|---|---|
| `declared` | The file stored an explicit unit string for this channel |
| `spec_default` | The unit is fixed by the format's specification |
| `unknown` | No unit information exists; `unit` is empty |

```sql
SELECT name, unit, unit_source
FROM telemetry_metadata('run.pds')
WHERE name IN ('Speed_Ref', 'STEER', 'P_F_BRAKE', 'gear');
```

```text
┌───────────┬───────┬──────────────┐
│ Speed_Ref │ m/s   │ spec_default │
│ STEER     │ rad   │ spec_default │
│ P_F_BRAKE │ Pa    │ spec_default │
│ gear      │       │ unknown      │
└───────────┴───────┴──────────────┘
```

**Cosworth/Pi PDS files store SI base units.** Speeds are m/s, angles and GPS coordinates are radians, pressures are Pa, temperatures are K, lengths are m, and accelerations are m/s². Engine speed is rad/s, not RPM. This is the convention Cosworth documents for Pi Toolbox, and PDS channel definitions carry a quantity code naming each channel's physical dimension, so units come from the file even in Pi Toolbox exports that strip the human-readable unit string.

Expect raw values to look unfamiliar as a result: a 269 km/h top speed reads `74.75`, a 75 bar brake pressure reads `7515500`, and 3 g of braking reads `-29.8`. Convert in SQL when you want other units:

```sql
SELECT max("Speed_Ref") * 3.6              AS top_speed_kmh,
       max("P_F_BRAKE") / 100000           AS brake_max_bar,
       min("I_ACCEL_LONG") / 9.80665       AS braking_g,
       degrees(min("STEER"))               AS steer_min_deg
FROM read_cosworth('run.pds');
```

If you convert the same channels repeatedly, see [Channel maps](#channel-maps-advanced) for a reusable way to do it.

Channels with `unit_source = 'unknown'` are genuinely unitless — counters, gear positions, lap numbers, flags, and ratios. A few files declare marker text such as `raw`, `flag`, or `pp1`; these are reported as `spec_default` because they label a channel without naming a convertible dimension.

### The unit registry

Every unit these formats use is registered, so a unit string is never just text. A native MQ12Di log with 1115 channels uses only 28 distinct unit spellings, so the vocabulary is small enough to enumerate: 87 canonical units across 25 physical dimensions.

`telemetry_metadata` reports the registry's view alongside the file's own spelling:

| Column | Meaning |
|---|---|
| `unit` | Exactly what the file says, unmodified |
| `canonical_unit` | Normalised spelling, or `NULL` if unrecognised |
| `dimension` | Physical dimension, or `NULL` if unrecognised |

This is what makes channels from different systems comparable. One file writes `sec`, another writes `s`; one writes `Lambda`, another `ratio`. They normalise to the same canonical unit and dimension:

```sql
SELECT unit AS file_unit, canonical_unit, dimension, count(*) AS channels
FROM telemetry_metadata('*.pds')
WHERE unit <> '' GROUP BY ALL ORDER BY channels DESC;
```

```text
┌───────────┬────────────────┬──────────────────┬──────────┐
│ file_unit │ canonical_unit │    dimension     │ channels │
├───────────┼────────────────┼──────────────────┼──────────┤
│ rad/s     │ rad/s          │ angular_velocity │        8 │
│ Lambda    │ ratio          │ ratio            │        3 │
│ sec       │ s              │ time             │        1 │
└───────────┴────────────────┴──────────────────┴──────────┘
```

Browse the whole registry with `telemetry_units()`, one row per spelling:

| Column | Meaning |
|---|---|
| `unit` | A spelling, canonical or alias |
| `canonical_unit` | The canonical spelling it resolves to |
| `is_canonical` | Whether `unit` is itself the canonical name |
| `dimension`, `base_unit` | Physical dimension and its SI base unit |
| `to_base_factor`, `to_base_offset` | `value_in_base = value * factor + offset` |
| `is_convertible` | False for markers and counts, which have no scale |

```sql
SELECT unit, canonical_unit, dimension, base_unit, to_base_factor
FROM telemetry_units() WHERE dimension = 'pressure' AND is_canonical;
```

### Converting between units

`telemetry_convert(value, from, to)` converts within a dimension and **errors rather than returning a wrong number** when the conversion makes no sense:

```sql
SELECT telemetry_convert(74.75, 'm/s', 'km/h');      -- 269.1
SELECT telemetry_convert(7515500, 'Pa', 'bar');      -- 75.155
SELECT telemetry_convert(-29.8, 'm/s^2', 'g');       -- -3.0387543
SELECT telemetry_convert(212, 'F', 'C');             -- 100.0  (affine, not just scaled)
```

```sql
SELECT telemetry_convert(1, 'm/s', 'bar');
-- Invalid Input Error: cannot convert 'm/s' (speed) to 'bar' (pressure):
--                      different physical dimensions

SELECT telemetry_convert(3, 'gear', '%');
-- Invalid Input Error: cannot convert 'gear' (count) to '%' (ratio):
--                      different physical dimensions

SELECT telemetry_convert(1, 'flag', 'raw');
-- Invalid Input Error: 'flag' is a marker, which has no scale to convert along

SELECT telemetry_convert(1, 'm/s', 'furlongs');
-- Invalid Input Error: unknown unit 'furlongs': not in the telemetry unit
--                      registry (see telemetry_units())
```

A gear position and a percentage are both "unitless", and converting between them is meaningless. Markers (`raw`, `flag`, `Driver`) and counts (`gear`, `laps`) therefore get their own dimensions and refuse to scale, so "unitless" is not a hole in the type system.

For direct wide-reader columns, opt into strict unit tags and let `telemetry_convert_column(column, to)` infer the source unit from the column type:

```sql
SELECT telemetry_convert_column("STEER", 'deg') AS "Steering Angle",
       telemetry_convert_column("RPM", 'rpm') AS "Engine RPM"
FROM read_cosworth(
    'run.pds',
    channels := 'STEER,RPM',
    unit_tags := true
);
```

A tagged column remains physically a `DOUBLE`, but its logical type is visible—for example, `typeof("STEER")` is `telemetry_unit:rad`. The tag is intentionally strict. Cast to `DOUBLE` for generic numeric functions, or convert it to an ordinary `DOUBLE` with `telemetry_convert_column`.

Inference refuses literals, expressions, unknown-unit channels, and mixed-file columns whose source units conflict:

```sql
SELECT telemetry_convert_column(1.0, 'deg');
-- Invalid Input Error: telemetry_convert_column requires a unit-tagged column ...
-- for a scalar or expression use telemetry_convert(value, from_unit, to_unit)
```

This separation keeps provenance explicit: use `telemetry_convert_column(tagged_column, to)` when the reader knows the unit, and `telemetry_convert(value, from, to)` for ordinary scalar expressions.

Use `telemetry_can_convert(from, to)` when you want a boolean instead of an error, for example to convert a column only where it is meaningful:

```sql
SELECT name, canonical_unit,
       CASE WHEN telemetry_can_convert(canonical_unit, 'km/h')
            THEN 'speed channel' ELSE 'leave alone' END AS treatment
FROM telemetry_metadata('run.pds') WHERE canonical_unit IS NOT NULL;
```

Because every unit is stored as an affine map to its dimension's SI base unit, any unit converts to any other unit of the same dimension without an N² table of pairs, and temperatures need no special-casing.

## Writing MoTeC files

`write_telemetry(source, output)` converts any supported input to a MoTeC LD file and writes its companion `.ldx` sidecar:

```sql
SELECT * FROM write_telemetry('run.pds', 'run.ld');
```

```text
┌─────────┬────────┬─────────┬──────────┬─────────┬──────────┬───────────┬───────────────┐
│ source  │ output │ format  │ channels │ samples │  bytes   │  sidecar  │ sidecar_bytes │
├─────────┼────────┼─────────┼──────────┼─────────┼──────────┼───────────┼───────────────┤
│ run.pds │ run.ld │ motec   │       31 │ 1716105 │ 13736960 │ run.ldx   │          2314 │
└─────────┴────────┴─────────┴──────────┴─────────┴──────────┴───────────┴───────────────┘
```

The writer is lossless or it refuses. Sample values keep their full precision — float64 channels are written as float64, and `u16`/`u32` widen rather than truncate — so a PDS round-trips through LD bit-for-bit. Rather than silently degrading a recording, it returns an error when a file cannot be represented: mixed sample rates in one output, non-contiguous chunks, non-integer frequencies, or channel names and units too long for LD's fixed-width fields.

The LDX records supplied session metadata and recovers beacon markers from dedicated lap-trigger channels when available, falling back to an increasing lap counter. It also writes total laps and the fastest complete beacon-to-beacon lap. Unknown metadata remains empty, and no lap markers are invented when the source has no reliable signal.

For a projected export, `channel_mapping` takes ordinary SQL nested lists. Each row is `[source, target name, target unit]`; the unit registry derives the complete affine conversion, including temperature offsets. `sum_channels` rows are `[left source, right source, target name, target unit]` and require identical source clocks and units:

```sql
SELECT * FROM write_telemetry('run.pds', 'sim-compatible.ld',
    channel_mapping := [
        ['Speed_Ref',  'Ground Speed',     'm/s'],
        ['RPM',        'Engine RPM',       'rpm'],
        ['P_F_BRAKE',  'Brake Pressure F', 'psi'],
        ['T_Brake_FL', 'Brake Temp FL',    '°C']
    ],
    sum_channels := [
        ['X_FL_DAMPER', 'X_FR_DAMPER', 'Damper Travel HF', 'mm']
    ]
);
```

This is SQL data rather than the optional channel-map mini-language, and no conversion constants are duplicated in the query. When either projection list is present, only declared output channels are written. See [`scripts/pds_to_sim_motec.sql`](scripts/pds_to_sim_motec.sql) for a complete environment-parameterized conversion.

Optional metadata named arguments: `driver`, `vehicle`, `vehicle_number`, `team`, `venue`, `event`, `session`, `comment`, `date`, `time`.

```sql
SELECT * FROM write_telemetry('run.pds', 'run.ld',
    driver := 'Tobi', vehicle := 'ORECA 07', venue := 'Sebring');
```

## Channel maps (advanced)

Everything above returns exactly what the file contains. A **channel map** is an opt-in layer that renames channels and converts their units — useful when you repeatedly load Cosworth SI data into a tool that expects km/h and degrees, or when you want one consistent schema across cars that name the same signal differently.

Skip this section unless you need it. Without `channel_map`, readers pass data through untouched.

Rules are one per line:

```text
source_channel -> target_name [unit] *scale +offset
```

Only the source channel is required; conversion is `value * scale + offset`.

```sql
SELECT * FROM read_cosworth('run.pds', rate := 20, channel_map := '
    Speed_Ref    -> Ground Speed  [km/h] *3.6
    STEER        -> Steered Angle [deg]  *57.29577951308232
    P_F_BRAKE    -> Brake Press   [bar]  *0.00001
    I_ACCEL_LONG -> G Force Long  [g]    *0.10197162129779283
    ACT          -> Air Temp      [C]    +-273.15
');
```

Keep a team's mapping in version control and pass the path instead:

```sql
SELECT * FROM read_cosworth('run.pds', channel_map := 'maps/oreca07.map');
```

`channel_map` works on `read_telemetry` (and the per-format readers), `telemetry_samples`, `telemetry_metadata`, and `telemetry_column_comments`. Units supplied by a map are reported as `declared`, since you stated them explicitly. Unmapped channels keep their original names, units, and values.

Mistakes fail at bind time rather than silently doing nothing:

```sql
SELECT * FROM read_cosworth('run.pds', channel_map := 'Speed_Reff -> Speed *3.6');
-- Binder Error: channel_map references channel(s) not present in the data: Speed_Reff
```

Mapping is deliberately explicit rather than automatic because files disagree in ways no heuristic can resolve. In one ORECA 07 export `gear` reads 1–6 while `gear_pos` reads 2–7 for the same physical gear, and the `FIA_Gps*` channels exist but are flat zero — an automatic mapper would have to guess, and would sometimes produce a plausible-looking but wrong trace.

### Persisting units alongside the data

DuckDB cannot attach comments to a table function's result columns, so unit metadata has to be applied to a materialised table or an exported file. `telemetry_column_comments(file, table)` generates everything needed for both:

| Column | Use |
|---|---|
| `ddl` | `COMMENT ON COLUMN` statement for a materialised table |
| `kv_metadata` | Unit plus `native_frequency_hz` and `native_sample_period_ns`, suitable for Parquet `KV_METADATA` |
| `channel_map_rule` | The `channel_map` rule reproducing this column's unit |

Two carriers are needed because neither alone covers every path out of DuckDB. Column comments live in the catalog and are queryable via `duckdb_columns()`, but **`COPY ... TO 'x.parquet'` drops them**. Parquet `KV_METADATA` travels with the file to other tools, but cannot annotate a DuckDB table.

The payload is self-describing, so a downstream reader can normalise and dimension-check without this extension:

```sql
SELECT column_name, kv_metadata
FROM telemetry_column_comments('run.pds', 'laps');
```

```text
┌──────────────────┬──────────────────────────────────────────────────────────────────────────┐
│ Alt_RPM          │ unit=rad/s; source=declared; dimension=angular_velocity; base_unit=rad/s; native_frequency_hz=100; native_sample_period_ns=10000000 │
│ DriverID         │ unit=; source=unknown; native_frequency_hz=10; native_sample_period_ns=100000000                                                       │
└──────────────────┴──────────────────────────────────────────────────────────────────────────┘
```

For a materialised table, run the generated DDL:

```sql
CREATE TABLE laps AS SELECT * FROM read_cosworth('run.pds', rate := 20);
-- execute each statement from telemetry_column_comments('run.pds', 'laps')
SELECT column_name, comment FROM duckdb_columns() WHERE table_name = 'laps';
```

For Parquet, pass the payloads as `KV_METADATA` so the units outlive DuckDB:

```sql
COPY laps TO 'laps.parquet' (FORMAT PARQUET, KV_METADATA {
  'Alt_RPM': 'unit=rad/s; source=declared; dimension=angular_velocity; base_unit=rad/s'
});
SELECT key::VARCHAR, value::VARCHAR FROM parquet_kv_metadata('laps.parquet');
```

`channel_map_rule` closes the loop the other way: rather than writing a map by hand, read the file's own units and use them as the starting point.

```sql
SELECT string_agg(channel_map_rule, '; ')
FROM telemetry_column_comments('run.pds', 'laps') WHERE unit <> '';
```

`tests/units_metadata_e2e.sh` proves the whole path on a real file with no hand-written unit strings, and `tests/unit_metadata_demo.sh` runs the channel-map variant.


## Recursive and mixed-format globs

`*`, `?`, character classes, and recursive `**` are supported. The extension also expands one `{a,b,c}` alternation of extension tokens, e.g. `{pds,ld,vbo,mp4,telemetry}`:

```sql
SELECT filename, max("Speed_Ref")
FROM read_telemetry(
    'data/**/*.{pds,ld,vbo,telemetry}',
    channels := 'Speed_Ref',
    filename := true
)
GROUP BY filename;
```

Quote patterns in a shell so the shell does not expand them first. Patterns are relative to the DuckDB process's working directory.

Every matched telemetry file must be parseable. Unknown extensions matched by a broad wildcard are ignored. The small Pi `Telemetry/` snapshot PDS layout in the development corpus is not yet supported; full `Offloaded/` PDS recordings are.

## Analysis examples

### Maximum speed per file and format

```sql
SELECT filename, max("Speed_Ref") * 3.6 AS vmax_kmh
FROM read_telemetry('**/*.pds', channels := 'Speed_Ref', filename := true)
GROUP BY filename
ORDER BY vmax_kmh DESC;
```

### Brake statistics at speed

```sql
WITH t AS (
    SELECT * FROM read_telemetry(
        'run.ld', rate := 100,
        channels := 'Corr Speed,P_F_BRAKE')
)
SELECT
    avg("P_F_BRAKE") FILTER ("Corr Speed" > 50) AS mean_brake,
    max("P_F_BRAKE") AS peak_brake
FROM t;
```

### Loaded throttle

```sql
SELECT avg("Driver Throttle Pos")
       FILTER (abs("G Force Lat") > 1.5) AS loaded_throttle
FROM read_telemetry(
    'run.ld', rate := 100,
    channels := 'Driver Throttle Pos,G Force Lat');
```

### Convert source telemetry directly to Parquet

```sql
COPY (
    SELECT * FROM read_telemetry(
        'weekend/**/*.vbo', rate := 50,
        channels := 'velocity kmh,latitude,longitude,heading',
        filename := true)
) TO 'vbox-weekend.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);
```

### Compare native logging configurations

```sql
SELECT format, name, unit,
       list(DISTINCT frequency_hz ORDER BY frequency_hz) AS rates,
       count(DISTINCT file) AS files
FROM telemetry_metadata('archive/**/*.{pds,ld,vbo}')
WHERE sample_count > 0
GROUP BY format, name, unit
ORDER BY name, format;
```

## Stats utility

The dependency-free Python utility exercises all three SQL layers and prints file inventory, native-rate distribution, raw statistics, and interpolated statistics:

```sh
./scripts/telemetry_stats.py run.pds
./scripts/telemetry_stats.py '**/*.{pds,ld,vbo}' --rate 50
./scripts/telemetry_stats.py run.ld \
  --channels 'Corr Speed,P_F_BRAKE,Gear' --rate 100
```

It invokes the DuckDB CLI; the Python `duckdb` package is not required.

## Install from GitHub Releases

The signed Community Extension installation above is recommended for native DuckDB. For manual or offline installation, release archives contain a platform-native file named exactly `motorsport_telemetry.duckdb_extension`.

### Linux x86-64

```sh
curl -LO https://github.com/tobi/duckdb_motorsport_telemetry/releases/latest/download/motorsport_telemetry-linux_amd64.zip
unzip motorsport_telemetry-linux_amd64.zip
duckdb -unsigned
```

```sql
INSTALL '/absolute/path/motorsport_telemetry.duckdb_extension';
LOAD motorsport_telemetry;
```

### Windows x86-64

```powershell
Invoke-WebRequest `
  https://github.com/tobi/duckdb_motorsport_telemetry/releases/latest/download/motorsport_telemetry-windows_amd64.zip `
  -OutFile motorsport_telemetry-windows_amd64.zip
Expand-Archive motorsport_telemetry-windows_amd64.zip
.\duckdb.exe -unsigned
```

```sql
INSTALL 'C:/absolute/path/motorsport_telemetry.duckdb_extension';
LOAD motorsport_telemetry;
```

### macOS Apple Silicon

Download `motorsport_telemetry-osx_arm64.zip`, extract it, start DuckDB with `-unsigned`, then use the same `INSTALL` and `LOAD` statements.

GitHub release artifacts are unsigned, so every process loading one must allow unsigned extensions. In Python:

```python
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("INSTALL '/path/motorsport_telemetry.duckdb_extension'")
con.execute("LOAD motorsport_telemetry")
print(con.sql("SELECT * FROM telemetry_metadata('run.vbo')"))
```

The signed Community Extension was first published via [upstream PR #2363](https://github.com/duckdb/community-extensions/pull/2363). The v0.7.0 descriptor is prepared in [`community-extension/description.yml`](community-extension/description.yml) for a follow-up PR; the release path remains useful for offline installs and historical versions, while this descriptor excludes DuckDB-Wasm, musl, MinGW, and RTools and the standalone repository remains the path for the separate DuckDB-Wasm build.

## Build from source

Requirements for native builds: Rust 1.85+, Python 3, DuckDB CLI 1.4.x.

```sh
git clone --recursive https://github.com/tobi/duckdb_motorsport_telemetry.git
cd duckdb_motorsport_telemetry
make test
make build
```

Output:

```text
build/release/motorsport_telemetry.duckdb_extension
```

Manual package command:

```sh
cargo build --release -p duckdb-motorsport-telemetry
python scripts/package_extension.py \
  --library target/release/libmotorsport_telemetry.so \
  --output build/release/motorsport_telemetry.duckdb_extension \
  --platform linux_amd64
```

Build the browser side module with Rust's Emscripten target and an activated emsdk:

```sh
source /path/to/emsdk/emsdk_env.sh
./scripts/build_wasm_extension.sh
cd web
bun install
bun run build
```

WASM outputs:

```text
build/wasm/wasm_eh/motorsport_telemetry.duckdb_extension.wasm
build/wasm/wasm_mvp/motorsport_telemetry.duckdb_extension.wasm
```

GitHub Actions workflows:

- `.github/workflows/ci.yml` — formatting, Clippy, native tests, synthetic SQL integration, WASM compilation, browser build, and headless Chromium PDS/MoTeC/VBO smoke tests
- `.github/workflows/release.yml` — Linux, Windows, macOS, `wasm_eh`, and `wasm_mvp` builds; release ZIPs; browser lab and extension-repository deployment

## Rust telemetry dependencies

The format parsers and shared telemetry model live in
[`motorsport-telemetry-rs`](https://github.com/tobi/motorsport-telemetry-rs).
This repository contains only the DuckDB adapter. Both native and WASM manifests
track the upstream `master` branch, with resolved commits recorded in their
lockfiles and refreshed automatically by Dependabot.

| Crate | Standalone library API | Native storage |
|---|---|---|
| `motorsport-telemetry-core` | `TelemetrySource`, `Channel`, `Chunk` | No file I/O |
| `cosworth-telemetry` | `CosworthFile::open` / `from_bytes` | `open` memory-maps PDS |
| `aim-telemetry` | `AimFile::open` / `from_bytes` | `open` memory-maps MP4 |
| `motec-telemetry` | `MotecFile::open` / `from_bytes` | `open` memory-maps LD |
| `racelogic-telemetry` | `RacelogicFile::open` / `from_bytes` / `from_slice` | `open` memory-maps VBO |
| `duckdb-motorsport-telemetry` | DuckDB table functions | Uses native parser mappings |

Use a parser without DuckDB:

```rust
use motorsport_telemetry_core::TelemetrySource;
use cosworth_telemetry::CosworthFile;

let file = CosworthFile::open("run.pds")?;
let speed = file.channels().iter()
    .position(|channel| channel.name == "Speed_Ref")
    .unwrap();

println!("first={}", file.decode(speed, 0, 0));
println!("at 10s={:?}", file.sample_at(speed, 10_000_000_000, true));
# Ok::<(), Box<dyn std::error::Error>>(())
```

The upstream repository contains standalone examples and parser-specific
documentation.

## Performance model

- binary files are memory-mapped
- exact scans decode directly into DuckDB vectors
- scans split into 2,048-row parallel tasks
- projection pushdown skips unused output and value decoding
- channel/time named arguments prune before decoding
- no object allocation per sample
- integer nanoseconds are the canonical clock

On a 25.5 MB PDS fixture, a selected-channel aggregate took about 2.8 ms in-process and 30 ms including DuckDB CLI startup. RacingMagick's full JavaScript parse of the same file took about 384 ms internally and roughly 517 MB peak RSS; the narrow DuckDB query used roughly 36 MB.

## Format notes and limitations

- PDS: marker and markerless definitions, native typed channels, compact export fallback, multi-chunk ordering
- MoTeC: LD channel data and physical conversion are supported; writing creates an `.ldx` with inferred lap markers, though reading existing `.ldx` files as a relation is not yet supported
- VBO: core sections, units, custom channels, midnight rollover, and irregular timestamps are supported
- remote/httpfs paths are not yet supported
- source units are preserved; values are not silently normalized
- calculated channels visible in vendor software but absent from a source file cannot be reconstructed automatically

The format implementations were developed against the reference parsers and specifications in RacingMagick.

## License

MIT
