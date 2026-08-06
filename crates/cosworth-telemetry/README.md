# cosworth-telemetry

Standalone memory-mapped Pi/Cosworth PDS parser. It supports marker and markerless definitions, typed values, compact exports, bounds checking, and authoritative chunk-table ordering.

## Library contract

`CosworthFile::open(path)` memory-maps a local PDS and parses metadata directly
from the mapping. `CosworthFile::from_bytes(path, bytes)` owns an in-memory
buffer for callers that already have the file bytes. Both constructors return
the same `CosworthFile` and `TelemetrySource` behavior; choose `open` for
native files and `from_bytes` for embedded input.

`cosworth_telemetry::read_metadata(path)` returns the shared `FileMetadata`
summary without DuckDB; `read_metadata_from_bytes` is the owned-buffer form.
PDS has no universally reliable absolute session key, so that field remains
empty unless the format exposes one.

```rust
use motorsport_telemetry_core::TelemetrySource;
use cosworth_telemetry::CosworthFile;

let file = CosworthFile::open("run.pds")?;
let speed = file.channels().iter().position(|c| c.name == "Speed_Ref").unwrap();
let first = file.decode(speed, 0, 0);
let at_one_second = file.sample_at(speed, 1_000_000_000, true);
# Ok::<(), Box<dyn std::error::Error>>(())
```

```sh
cargo run -p cosworth-telemetry --example inspect_cosworth -- run.pds
```
