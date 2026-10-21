# DuckDB 2.0 Community Extension submission

The native adapter now targets exactly **DuckDB 2.0.0** through the experimental
C API v2 (`duckdb-neo`, upstream Rust commit
`e1b0940dfddd575c43fe4e2f070d47ee53d419cd`). The core build is
`d54cbbe3df17e8e5ce68312f483b4155836112aa`. Artifacts carry
`C_STRUCT_UNSTABLE`, not the API-v1 `C_STRUCT` footer.

After the corresponding source revision is reviewed, built, signed and published
by [DuckDB Community Extensions](https://github.com/duckdb/community-extensions):

```sql
INSTALL AND LOAD motorsport_telemetry FROM community;
```

The descriptor in [description.yml](../community-extension/description.yml) is
submission metadata, not evidence that a 2.0 artifact is already published.
For local validation, use an unsigned DuckDB 2.0.0 session and
`INSTALL AND LOAD '/absolute/path/motorsport_telemetry.duckdb_extension'`.

## Build and validation contract

- Rust 1.88+ and uv; pinned `extension-ci-tools` submodule.
- Standard `configure`, `release`, `test_release` targets; `TARGET_DUCKDB_VERSION=v2.0.0`.
- Pin the sqllogictest runner to DuckDB 2.0.0 and force `SKIP_TESTS=0` in CI.
- `test/sql/registration.test` checks registration without proprietary recordings.
- `tests/api_v2.sh` checks combined installation, documentation, NULLs, multi-chunk vectors and conversion errors.
- `tests/integration.sh` checks parser reads, projections, interpolation, units,
  sessions, normalization, MoTeC exports and compressed MTJ round trips. It uses
  synthetic fixtures and the writer from the same pinned telemetry commit.
- Native telemetry parsers use `7cb37dc61eeb343d99eeeb27a786970ff2ec26bc`.
  The current library reads JSONL/zstd `.telemetry`; legacy ZIP was removed upstream.

The descriptor excludes Wasm, musl, MinGW and RTools. Native release CI includes
Linux amd64/arm64, macOS arm64 and Windows amd64. Local validation on one platform
does not replace that upstream matrix.

## Browser compatibility

The latest DuckDB-Wasm package still embeds 1.5.4. Its independent workspace keeps
the last compatible extension v1.3.5 at
`4eb038b5d7e7cdadc758cd4d4e7e7fbf890e052a`. Its `C_STRUCT` artifacts remain
under `v1.5.4/wasm_eh` and `v1.5.4/wasm_mvp`; native v2 artifacts are under
`v2.0.0/<platform>/`. Do not publish a native v2 binary into a Wasm v1 path.

## Publication

Pin `repo.ref` to the reviewed source commit, push it, run the platform matrix,
then copy the descriptor into `extensions/motorsport_telemetry/description.yml`
in a Community Extensions pull request. Acceptance and signing are external
steps; the local migration does not modify the published community artifact.
The historical [PR #2363](https://github.com/duckdb/community-extensions/pull/2363)
first published v0.6.1 and is not the v2 submission.
