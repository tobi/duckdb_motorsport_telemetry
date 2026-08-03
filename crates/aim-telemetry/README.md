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

The parser intentionally does not guess how to flatten aggregate AiM records such as `GPS0` and `LapPk` into scalar channels. Scalar channels without a safely decoded unit remain unitless with `unit_source = unknown`.

```rust
use aim_telemetry::AimFile;
use motorsport_telemetry_core::TelemetrySource;

let recording = AimFile::open("smartycam.mp4")?;
for channel in recording.channels() {
    println!("{}: {:?} Hz", channel.name, channel.frequency_hz());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```
