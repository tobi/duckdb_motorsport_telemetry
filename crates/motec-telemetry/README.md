# motec-telemetry

Standalone memory-mapped MoTeC `.ld` parser. It validates the LD header, walks channel metadata, decodes float and integer channels, and applies MoTeC scale/shift/multiplier conversion.

```rust
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::TelemetrySource;

let file = MotecFile::open("run.ld")?;
println!("{} / {} / {}", file.driver, file.vehicle, file.venue);
for channel in file.channels() {
    println!("{}: {:?} Hz", channel.name, channel.frequency_hz());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The companion `.ldx` remains available to applications for lap markers; sample parsing does not require it.

```sh
cargo run -p motec-telemetry --example inspect_motec -- run.ld
```
