---
name: duckdb-motorsport-telemetry
description: Query Pi/Cosworth PDS, MoTeC LD, and Racelogic VBO telemetry directly in DuckDB. Use when inspecting telemetry channels, extracting native samples, resampling mixed-rate channels, comparing files, converting telemetry to Parquet, or pruning telemetry archives by file creation date.
---

# DuckDB Motorsport Telemetry

Use the extension's exact metadata/sample relations before choosing a wide resampling rate. Preserve source units unless the analysis explicitly converts them.

## Load

Published builds are unsigned. Start DuckDB 1.4.3 with `duckdb -unsigned`, then install over HTTPS:

```sql
INSTALL motorsport_telemetry
FROM 'https://tobi.github.io/duckdb_motorsport_telemetry';
LOAD motorsport_telemetry;
```

For a manually downloaded release artifact, use `INSTALL '/absolute/path/motorsport_telemetry.duckdb_extension'` instead.

## Workflow

1. Discover exact names, units, rates, and whether channels actually have samples:

```sql
SELECT format, name, unit, frequency_hz, sample_count
FROM telemetry_metadata('run.pds')
ORDER BY name;
```

A definition with `sample_count = 0` is not recorded data.

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

- `read_telemetry` — auto-detect `.pds`, `.ld`, `.vbo`; supports mixed-format globs
- `read_cosworth` — Pi/Cosworth PDS
- `read_motec` — MoTeC LD
- `read_vbo` — Racelogic VBOX VBO

## Pushdown rules

Always put physical selections in named arguments:

- `channel` for exact long-form scans
- `channels` for wide scans
- `start_ns`, `end_ns` for session time
- `create_date_from`, `create_date_to` for archive file dates

DuckDB pushes projected columns into the extension, so do not use `SELECT *` when only two channels are needed. DuckDB 1.4 does not pass arbitrary `WHERE` filters to public-C-API table functions; a `WHERE` clause alone does not prevent decoding/opening.

```sql
SELECT filename, max("Speed_Ref")
FROM read_cosworth(
  '**/Offloaded/*.pds',
  channels := 'Speed_Ref',
  filename := true,
  add_create_date_column := true,
  create_date_from := TIMESTAMP '2026-07-01',
  create_date_to := TIMESTAMP '2026-08-01')
GROUP BY filename;
```

## Interpolation

- Default: `interpolate := 'linear'`
- Continuous float channels interpolate linearly.
- Gear, lap counters/beacons, switches, flags, status, state, alarms, and integer channels step/forward-fill.
- Use `interpolate := 'previous'` to force step interpolation for all channels.
- Keep integer nanoseconds as the canonical clock.

## Globs

Recursive globs and `{pds,ld,vbo}` are supported:

```sql
SELECT * FROM telemetry_metadata('weekend/**/*.{pds,ld,vbo}');
```

Quote globs in shell commands. Unknown extensions from broad globs are ignored; malformed files with a supported extension fail the bind. Small Pi `Telemetry/` snapshot PDS files are currently unsupported, so prefer `**/Offloaded/*.pds` for that corpus.

## Units

Values and units are source-exact. Do not silently combine:

- m/s and km/h
- Pa and bar
- ratio and percent
- m/s² and g

Convert explicitly, e.g. `speed_mps * 3.6`, `pressure_pa / 100000`, or `accel_mps2 / 9.80665`.

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
