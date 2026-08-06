# vbo-telemetry

Standalone Racelogic VBOX `.vbo` parser. It handles section-based files, optional column names and units, UTC time-of-day conversion, midnight rollover, irregular timestamps, and custom channels.

## Library contract

`VboFile::open(path)` memory-maps the local text file and parses its sections
from borrowed slices; parsed numeric columns are then owned by `VboFile`.
`VboFile::from_bytes(path, bytes)` and `VboFile::from_slice(path, bytes)` are
the in-memory entry points. All constructors expose the same
`TelemetrySource` contract.

`vbo_telemetry::read_metadata(path)` returns the shared `FileMetadata` summary,
including the time-of-day clock, lap and driver channels when present, and the
native AVI linkage fields. `read_metadata_from_bytes` provides the same summary
for owned input.

```rust
use motorsport_telemetry_core::TelemetrySource;
use vbo_telemetry::VboFile;

let file = VboFile::open("run.vbo")?;
let velocity = file.channels().iter().position(|c| c.name == "velocity kmh").unwrap();
println!("first speed={}", file.decode(velocity, 0, 0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

```sh
cargo run -p vbo-telemetry --example inspect_vbo -- run.vbo
```
