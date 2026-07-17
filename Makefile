DUCKDB ?= duckdb
PROFILE ?= release
EXTENSION_VERSION ?= v0.4.0
PLATFORM ?= $(shell $(DUCKDB) -csv -noheader -c "PRAGMA platform;")
CARGO_FLAGS := $(if $(filter release,$(PROFILE)),--release,)
TARGET_DIR := target/$(PROFILE)
BUILD_DIR := build/$(PROFILE)
SHARED_LIB := $(TARGET_DIR)/libmotorsport_telemetry.$(if $(filter Darwin,$(shell uname -s)),dylib,so)
EXTENSION := $(BUILD_DIR)/motorsport_telemetry.duckdb_extension

.PHONY: all build test integration-test clean

all: build

build:
	cargo build $(CARGO_FLAGS)
	mkdir -p $(BUILD_DIR)
	cp $(SHARED_LIB) $(EXTENSION)
	python3 scripts/append_metadata.py $(EXTENSION) --platform $(PLATFORM) --extension-version $(EXTENSION_VERSION)
	@echo "Built $(EXTENSION) for $(PLATFORM)"

test:
	cargo fmt --check
	cargo test
	$(MAKE) integration-test

integration-test: build
	DUCKDB=$(DUCKDB) EXTENSION=$(abspath $(EXTENSION)) tests/integration.sh

clean:
	cargo clean
	rm -rf build
