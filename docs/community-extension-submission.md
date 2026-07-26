# DuckDB Community Extension submission

DuckDB Community Extensions are submitted by pull request to
[`duckdb/community-extensions`](https://github.com/duckdb/community-extensions). The pull request adds one descriptor at
`extensions/motorsport_telemetry/description.yml`; DuckDB's infrastructure checks out the pinned source commit, builds it,
tests it, signs it, and publishes it for each supported DuckDB platform.

Once accepted, installation becomes:

```sql
INSTALL motorsport_telemetry FROM community;
LOAD motorsport_telemetry;
```

DuckDB no longer needs `-unsigned`, `httpfs`, or this project's custom repository for Community Extension builds.

## Repository preparation completed

- The repository uses DuckDB's stable public C extension API.
- `extension-ci-tools` is pinned as a git submodule.
- The root `Makefile` implements the standard `configure`, `debug`, `release`, `test_debug`, and `test_release` targets.
- CI builds through the Community Extension toolchain against DuckDB 1.5.4.
- `test/sql/registration.test` verifies that a signed-style build loads and exposes all six table functions without shipping proprietary telemetry.
- Parser and integration tests continue to generate all telemetry fixtures at runtime.

The standalone custom repository remains useful for historical versions and DuckDB-Wasm, because the Community Extension repository exposes only the descriptor's currently pinned source revision and does not publish WASM builds.

## Submitted descriptor

The submitted descriptor is [`community-extension/description.yml`](../community-extension/description.yml). It pins version
`0.6.0` at commit `b0a16ce6f79af7672a031524132414b848de8ff8` (tag `v0.6.0`) and excludes DuckDB-Wasm, musl, MinGW, and
RTools; native Linux, macOS, and Windows MSVC are enabled.

The upstream pull request adds that file as `extensions/motorsport_telemetry/description.yml` in
`duckdb/community-extensions`. Remaining work is to answer review comments and fix any platform build failures reported
by the upstream matrix.

To submit a later revision:

1. Set `repo.ref` to the exact reviewed commit on `tobi/duckdb_motorsport_telemetry` and bump the descriptor version.
2. Run the repository CI and verify the Community Extension build job.
3. Copy the descriptor into the `duckdb/community-extensions` fork and open a pull request.

## Maintenance model

Community Extensions pin `repo.ref` to one source commit. Shipping an update requires a small upstream pull request that changes the descriptor version and ref. DuckDB rebuilds all descriptors for each new DuckDB release. The extension's own latest-DuckDB CI should remain green so incompatibilities are found before a DuckDB release freeze.
