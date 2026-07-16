# duckdb-pds

A fast Rust DuckDB extension for Pi Research/Cosworth `.pds` motorsport telemetry.
It memory-maps files, parses channel/chunk metadata once per table-function bind,
decodes directly into DuckDB vectors, and never materializes an object per sample.

The parser is based on the PDS implementation and format work in
[`racingmagick`](../racingmagick). In particular, it preserves **chunk-index
table order**. Sorting chunks by their `order` field or data pointer corrupts
some interrupted native recordings.

## Build

Requirements: Rust 1.84+, Python 3, DuckDB CLI 1.4.x.

```sh
make build
# build/release/pds.duckdb_extension
```

Local builds are unsigned:

```sql
-- Start the CLI with: duckdb -unsigned
LOAD './build/release/pds.duckdb_extension';
```

`make test` runs formatting, Rust tests, and a DuckDB integration test against a
synthetic multi-chunk PDS file.

## SQL API

### Inspect channels

```sql
SELECT name, unit, data_type, frequency_hz, sample_count, chunk_count
FROM pds_channels('run.pds')
WHERE sample_count > 0
ORDER BY name;
```

`pds_channels(path)` returns one row per definition, including definitions with
no sample chunks. `path` may be a local glob.

Columns: `file`, `channel_id`, `name`, `unit`, `type_code`, `data_type`,
`frequency_hz`, `sample_period_ns`, `sample_count`, `chunk_count`, `duration_ns`.

### Exact raw samples

```sql
SELECT time_ns, value
FROM pds_samples(
  'run.pds',
  channel := 'Speed_Ref',
  start_ns := 519000000000,
  end_ns := 524000000000
)
ORDER BY time_ns;
```

`pds_samples` preserves mixed sample rates and stored values. Multiple channel
names can be comma-separated. Output columns are:

- `file`, `channel_id`, `channel`, `unit`, `frequency_hz`
- `sample_index`: index in the channel after concatenating chunks in table order
- `time_ns`: integer nanoseconds from that channel's first sample
- `value`: stored value converted to DuckDB `DOUBLE`; no unit normalization

Time bounds are half-open: `start_ns <= time_ns < end_ns`.

### Friendly wide scan

```sql
SELECT
  time_ns,
  "Speed_Ref",
  "I_ACCEL_LONG"
FROM read_pds(
  'run.pds',
  rate := 100,
  channels := 'Speed_Ref,I_ACCEL_LONG',
  interpolate := 'linear',
  start_ns := 500000000000,
  end_ns := 530000000000
);
```

`read_pds` creates a dynamic wide schema from the selected PDS channel names.
Defaults are `rate := 100` and `interpolate := 'previous'`. Linear
interpolation applies only to floating-point channels; integer/discrete
channels remain previous-sample. Across globs, channels are unioned by
case-insensitive name and absent channels are `NULL`.

## Pushdown and performance

Projection pushdown is enabled on all three table functions:

```sql
SELECT "Speed_Ref"
FROM read_pds('run.pds', channels := 'Speed_Ref,I_ACCEL_LONG');
```

Only `Speed_Ref` is decoded. In `pds_samples`, `count(*)` does not decode sample
values. Scans are split into 2,048-row tasks and can run on multiple DuckDB
worker threads. Input is memory-mapped and sample values are decoded directly
into output vectors.

DuckDB 1.4's public C table-function API exposes projection pushdown but not SQL
filter-expression pushdown. Therefore use the bind-time arguments for physical
predicate pushdown:

- `channel` on `pds_samples`
- `channels` on `read_pds`
- `start_ns` and `end_ns` on both

A `WHERE time_ns ...` predicate is still correct, but DuckDB applies it after
the scan. Passing the same bounds as named arguments skips whole chunks and
sample ranges before decoding.

On the 78.9 MB Mosport Run 1 fixture, a release build scanned 112,000
`Speed_Ref` samples for `count/min/max` in about **35 ms total CLI wall time** on
the development machine. A projected 100 Hz two-channel wide aggregate over
224,000 rows was also about **35 ms**; process startup dominates these figures.

## Format coverage

Implemented:

- little-endian PDS directory probing
- marker (`0x7c72`) and markerless channel definitions
- native typed samples: `u8`, `i16`, `u16`, `i32`, `u32`, `f32`, `f64`
- duplicate-channel-id chunks and compact export chunk fallback
- multi-chunk channels in authoritative chunk-table order
- local files and local globs
- bounds checking and sample-count clamping

Not yet implemented:

- remote/httpfs paths
- embedded lap/event relations
- automatic unit normalization or canonical channel aliases
- compressed PDS families not represented by the current format variants
- the small (~4–14 MB) `MQ12Di/.../Telemetry/` snapshot layout seen in this
  workspace; `Offloaded/` PDS recordings are supported
- signed Community Extension packaging

## Development

```sh
cargo fmt --check
cargo test
make integration-test
```

The parser is isolated in `src/parser.rs`; DuckDB-specific vectorized adapters
are in `src/lib.rs`.
