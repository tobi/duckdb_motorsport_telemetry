# DuckDB Motorsport Telemetry

A fast, vectorized DuckDB extension and reusable Rust parser workspace for:

- Pi Research / Cosworth **PDS** (`.pds`)
- MoTeC i2 **LD** (`.ld`)
- Racelogic VBOX **VBO** (`.vbo`)

The exact model is two relations—channel metadata and native-rate samples—plus a friendly interpolated wide reader. Files are memory-mapped where possible, values decode directly into DuckDB vectors, scans are parallel, and projection pushdown avoids decoding channels a query does not use.

## Easiest installation

Start DuckDB 1.4.3 with unsigned extensions enabled:

```sh
duckdb -unsigned
```

Enable DuckDB's signed HTTPS filesystem extension, then install directly from this project's extension repository:

```sql
INSTALL httpfs;
LOAD httpfs;
INSTALL motorsport_telemetry
FROM 'https://pages.tobi.lutke.com/duckdb_motorsport_telemetry';
LOAD motorsport_telemetry;
```

Verify it against a telemetry file:

```sql
SELECT name, unit, frequency_hz, sample_count
FROM telemetry_metadata('/path/to/run.pds');
```

DuckDB downloads the platform-specific `.duckdb_extension.gz`, decompresses it, and installs it in the normal extension directory. `httpfs` is needed only because the repository uses HTTPS. Subsequent sessions only need:

```sql
LOAD motorsport_telemetry;
```

The repository artifacts are unsigned, so every DuckDB process loading the extension must still allow unsigned extensions. See [Install from GitHub Releases](#install-from-github-releases) for manual ZIP installation and Python usage.

## Browser telemetry lab

Open **[Telemetry Lab](https://pages.tobi.lutke.com/duckdb_motorsport_telemetry/)** to analyze a recording without installing anything. Drop a `.pds`, `.ld`, or `.vbo` file into the page; the file stays in your browser and is never uploaded.

The lab runs this same Rust extension as a DuckDB-Wasm side module and automatically shows:

- recorded versus empty channel definitions
- exact native rates, sample counts, units, and duration
- likely speed, brake, throttle, acceleration, and gear signals
- exact min/mean/max statistics and a quick synchronized trace
- a full SQL workbench with useful starter queries

The browser smoke test generates synthetic PDS, MoTeC, and VBO files at runtime, drops each into Chromium, verifies parsing, and executes SQL. No real telemetry fixture is committed.

## SQL in 30 seconds

```sql
LOAD motorsport_telemetry;

-- Discover channels and native rates.
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

## Functions

### `telemetry_metadata(path)`

One row per channel definition:

| Column | Meaning |
|---|---|
| `file` | Full input path |
| `format` | `pds`, `motec`, or `vbo` |
| `channel_id` | File-local channel identifier |
| `name`, `unit` | Original channel metadata |
| `type_code`, `data_type` | Stored representation |
| `frequency_hz`, `sample_period_ns` | Native clock |
| `sample_count`, `chunk_count`, `duration_ns` | Physical storage summary |

```sql
SELECT file, format, count(*) AS definitions,
       count(*) FILTER (sample_count > 0) AS sampled_channels,
       sum(sample_count) AS raw_samples
FROM telemetry_metadata('weekend/**/*.{pds,ld,vbo}')
GROUP BY file, format;
```

Find lateral acceleration availability:

```sql
SELECT file, name, unit, frequency_hz, sample_count
FROM telemetry_metadata('**/*.{pds,ld,vbo}')
WHERE lower(name) SIMILAR TO '%(lat|lateral)%(accel|g)%'
ORDER BY file, name;
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
- `channels := 'A,B,C'` — schema and physical channel selection
- `interpolate := 'linear'` or `'previous'`
- `start_ns`, `end_ns` — physical time-range pruning
- `filename := true` — add a `filename` column
- `add_filename_as_column := true` — explicit alias for `filename`
- `add_create_date_column := true` — add filesystem creation timestamp (modified-time fallback)
- `create_date_from := TIMESTAMP '2026-07-01'` — inclusive pre-open file pruning
- `create_date_to := TIMESTAMP '2026-08-01'` — exclusive pre-open file pruning

Continuous floating-point channels use linear interpolation by default, including contiguous chunk boundaries. Integer and known discrete/event channels—gear, lap number/beacon, switches, status, state, flags, alarms, GPS solution type—use previous-value semantics.

Across multiple files, schemas union by case-insensitive channel name. Missing channels are `NULL`.

Creation-date pruning happens before telemetry files are opened:

```sql
SELECT filename, create_date, max("Speed_Ref")
FROM read_cosworth(
    'archive/**/*.pds',
    channels := 'Speed_Ref',
    filename := true,
    add_create_date_column := true,
    create_date_from := TIMESTAMP '2026-07-01',
    create_date_to := TIMESTAMP '2026-08-01')
GROUP BY filename, create_date;
```

A plain `WHERE create_date ...` remains logically correct but is applied after scanning because DuckDB 1.4's public C table-function API does not expose arbitrary filter expressions to extensions. Use `create_date_from`/`create_date_to` when physical pushdown matters.

## Recursive and mixed-format globs

`*`, `?`, character classes, and recursive `**` are supported. The extension also expands the convenient `{pds,ld,vbo}` suffix group:

```sql
SELECT filename, max("Speed_Ref")
FROM read_telemetry(
    'data/**/*.{pds,ld,vbo}',
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

The HTTPS `INSTALL ... FROM` command above is recommended. For manual or offline installation, release archives contain a platform-native file named exactly `motorsport_telemetry.duckdb_extension`.

### Linux x86-64

```sh
curl -LO https://github.com/tobi/duckdb_motorsport_telemetry/releases/download/v0.2.0/motorsport_telemetry-linux_amd64.zip
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
  https://github.com/tobi/duckdb_motorsport_telemetry/releases/download/v0.2.0/motorsport_telemetry-windows_amd64.zip `
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

GitHub artifacts are unsigned, so every process loading one must allow unsigned extensions. In Python:

```python
import duckdb

con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("INSTALL '/path/motorsport_telemetry.duckdb_extension'")
con.execute("LOAD motorsport_telemetry")
print(con.sql("SELECT * FROM telemetry_metadata('run.vbo')"))
```

A future DuckDB Community Extension submission will enable:

```sql
INSTALL motorsport_telemetry FROM community;
LOAD motorsport_telemetry;
```

## Build from source

Requirements for native builds: Rust 1.84+, Python 3, DuckDB CLI 1.4.x.

```sh
git clone https://github.com/tobi/duckdb_motorsport_telemetry.git
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

## Reusable Rust crates

This is one Cargo workspace; DuckDB is only the adapter layer.

| Crate | Purpose |
|---|---|
| `motorsport-telemetry-core` | Shared channel/chunk model, exact samples, interpolation |
| `cosworth-telemetry` | Memory-mapped Pi/Cosworth PDS parser |
| `motec-telemetry` | Memory-mapped MoTeC LD parser and conversion formula |
| `vbo-telemetry` | Racelogic VBO text parser with irregular timestamps |
| `duckdb-motorsport-telemetry` | Vectorized DuckDB table functions |

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

Run the standalone inspectors:

```sh
cargo run -p cosworth-telemetry --example inspect_cosworth -- run.pds
cargo run -p motec-telemetry --example inspect_motec -- run.ld
cargo run -p vbo-telemetry --example inspect_vbo -- run.vbo
```

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
- MoTeC: LD channel data and physical conversion are supported; `.ldx` lap markers are not yet exposed as a relation
- VBO: core sections, units, custom channels, midnight rollover, and irregular timestamps are supported
- remote/httpfs paths are not yet supported
- source units are preserved; values are not silently normalized
- calculated channels visible in vendor software but absent from a source file cannot be reconstructed automatically

The format implementations were developed against the reference parsers and specifications in RacingMagick.

## License

MIT
