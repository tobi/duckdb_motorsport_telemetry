# aim-telemetry

Native reader for AiM Sports telemetry stored as an `aimd` track inside an ISO Base Media/MP4 recording.

## Design

- Memory-maps the MP4. It never reads or decodes the video or audio tracks.
- Locates telemetry by the `aimd` sample-entry FourCC, not by track number or the localized handler name.
- Resolves samples from standard `stsd`, `stts`, `stsc`, `stsz`, and `stco`/`co64` tables.
- Reads channel names, record IDs, widths, and layout from AiM `CHS` schema blocks.
- Reads scalar `(S … )` records using their declared record IDs and timestamps.
- Uses the modal recorded timestamp delta as each channel's native frequency, while retaining acquisition gaps as separate chunks.
- Rejects an MP4 immediately with `NoAimdTrack` when no `aimd` sample entry exists.

See [`FORMAT.md`](FORMAT.md) for the documented MP4 tables, packet framing, `CHS` schema fields, scalar record layout, and compatibility policy.

`GPS0` is decoded into geodetic position, speed, heading, accuracy, satellite, timing and status channels at its native 25 Hz. `LapPk` remains unexpanded: it is defined but has no payload in any of the five available recordings, while lap number and lap timing are carried by ordinary scalar channels. Scalar channels without a safely decoded unit remain unitless with `unit_source = unknown`.

The standalone library exposes `AimFile::open(path)` for local native MP4s;
it memory-maps the file. `AimFile::from_bytes(path, bytes)` is the equivalent
owned-buffer constructor for callers that already have MP4 bytes.

## DuckDB integration

The native DuckDB adapter exposes this parser through:

```sql
SELECT * FROM telemetry_metadata('session.mp4');
SELECT * FROM telemetry_samples(
  'session.mp4',
  channel := 'RPM,GPS Speed,GPS Latitude,GPS Longitude'
);
SELECT * FROM read_aim(
  'session.mp4',
  channels := 'RPM,GPS Speed,GPS Latitude,GPS Longitude',
  rate := 10
);
```

`read_aim` and `read_aimd` are AiM-specific wide-reader aliases;
`read_telemetry('session.mp4', ...)` auto-detects the `aimd` track. These
entry points are native-only in this workspace. The DuckDB-Wasm build does not
link the AiM parser, so `.mp4` inputs and the `read_aim`/`read_aimd` functions
are unavailable in the browser.

`GPS0` expands into 15 derived channels: `GPS Latitude`, `GPS Longitude`,
`GPS Altitude`, `GPS Speed`, `GPS Heading`, `GPS Satellites`,
`GPS Position Accuracy`, `GPS Speed Accuracy`, `GPS ECEF Velocity X`,
`GPS ECEF Velocity Y`, `GPS ECEF Velocity Z`, `GPS iTOW`, `GPS Week`,
`GPS DOP`, and `GPS Fix Flags`.

The reader does not decode video or audio, does not reconstruct `LapPk` without
a payload, and leaves scalar units unknown when the `CHS` record does not
provide a validated quantity. See [`FORMAT.md`](FORMAT.md) for the exact
packet and field compatibility rules.

```rust
use aim_telemetry::AimFile;
use motorsport_telemetry_core::TelemetrySource;

let recording = AimFile::open("smartycam.mp4")?;
for channel in recording.channels() {
    println!("{}: {:?} Hz", channel.name, channel.frequency_hz());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```
