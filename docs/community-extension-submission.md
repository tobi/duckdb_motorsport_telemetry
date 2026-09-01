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
- `test/sql/registration.test` verifies that a signed-style build loads and exposes all thirteen table functions without shipping proprietary telemetry.
- Adapter integration tests consume deterministic synthetic fixtures from `motorsport-telemetry-rs`; browser smoke tests generate their own temporary fixtures.

The prepared descriptor excludes DuckDB-Wasm, musl, MinGW, and RTools. The standalone custom repository remains useful for the project's separate DuckDB-Wasm build and historical versions.

## Prepared descriptor

The v1.3.3 descriptor is [`community-extension/description.yml`](../community-extension/description.yml). It pins version
`1.3.3` at commit `0f7b0472e6a2356c477228cd4b86f308d85afdd8` (tag `v1.3.3`) and excludes DuckDB-Wasm, musl, MinGW, and
RTools; native Linux, macOS, and Windows MSVC are enabled.

The prior upstream pull request, [#2363](https://github.com/duckdb/community-extensions/pull/2363), published version
`0.6.1`. The v1.3.3 descriptor is ready for a follow-up pull request that adds
`extensions/motorsport_telemetry/description.yml` with the new source ref.

Remaining work is to run the upstream matrix, open the follow-up pull request, and answer any review comments or platform
build failures it reports.

To submit a later revision:

1. Set `repo.ref` to the exact reviewed commit on `tobi/duckdb_motorsport_telemetry` and bump the descriptor version.
2. Run the repository CI and verify the Community Extension build job.
3. Copy the descriptor into the `duckdb/community-extensions` fork and open a pull request.

## Maintenance model

Community Extensions pin `repo.ref` to one source commit. Shipping an update requires a small upstream pull request that changes the descriptor version and ref. DuckDB rebuilds all descriptors for each new DuckDB release. The extension's own latest-DuckDB CI should remain green so incompatibilities are found before a DuckDB release freeze.
