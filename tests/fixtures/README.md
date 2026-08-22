# Adapter fixtures

Parser fixtures are owned by [`motorsport-telemetry-rs`](https://github.com/tobi/motorsport-telemetry-rs).

For local development, `telemetry-rs` is a symlink to a sibling checkout. CI checks out the parser repository into that path before running integration tests. This adapter intentionally keeps no duplicate format fixtures.
