EXTENSION_NAME=motorsport_telemetry
USE_UNSTABLE_C_API=1
# C API v2 is experimental and tied to this exact DuckDB release.
TARGET_DUCKDB_VERSION?=v2.0.0
DUCKDB_TEST_VERSION?=2.0.0
PYTHON_BIN?=uv run --no-project --python 3.12 python
EXTENSION_VERSION?=$(shell cat VERSION)
DUCKDB?=duckdb

include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

.PHONY: all build configure debug release test test_debug test_release integration-test clean sync-version

all: release
build: release

configure: sync-version venv platform extension_version

sync-version:
	uv run --no-project scripts/sync_version.py
	mkdir -p configure
	printf '%s\n' '$(EXTENSION_VERSION)' > configure/extension_version.txt

# The build and test targets stay separate, exactly as the Community Extension
# workflow invokes them: `configure`/`configure_ci`, then `release`, then
# `test_release`. A test target must never rebuild, because the same working
# tree is built inside a container and tested on the host, and a rebuild would
# then hit a `target/` directory owned by the other user.
debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test_debug: test_extension_debug
test_release: test_extension_release

test:
	cargo fmt --check
	cargo test --workspace
	cargo clippy --workspace --all-targets -- -D warnings
	$(MAKE) integration-test

integration-test:
	$(MAKE) configure
	$(MAKE) release
	DUCKDB=$(DUCKDB) EXTENSION=$(abspath build/release/motorsport_telemetry.duckdb_extension) tests/integration.sh
	DUCKDB=$(DUCKDB) EXTENSION=$(abspath build/release/motorsport_telemetry.duckdb_extension) tests/api_v2.sh

clean: clean_build clean_rust
