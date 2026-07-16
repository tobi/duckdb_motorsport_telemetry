#!/usr/bin/env python3
"""Append an unsigned DuckDB extension footer.

DuckDB's loader validates this footer even when started with -unsigned. The
256-byte signature is intentionally zero; distribution builds should replace
it through DuckDB's extension signing pipeline.
"""
from __future__ import annotations

import argparse
from pathlib import Path


def field(value: str) -> bytes:
    raw = value.encode("utf-8")
    if len(raw) > 32:
        raise ValueError(f"metadata field is longer than 32 bytes: {value!r}")
    return raw.ljust(32, b"\0")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("extension", type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--api-version", default="v1.2.0")
    parser.add_argument("--extension-version", default="v0.1.0")
    args = parser.parse_args()

    # Same 534-byte section emitted by DuckDB's scripts/append_metadata.cmake.
    custom_section = b"\0" + bytes((147, 4, 16)) + b"duckdb_signature" + bytes((128, 4))
    fields = [
        "4",
        args.platform,
        args.api_version,
        args.extension_version,
        "C_STRUCT",
        "",
        "",
        "",
    ]
    custom_section += b"".join(field(value) for value in reversed(fields))
    custom_section += bytes(256)
    if len(custom_section) != 534:
        raise AssertionError(len(custom_section))
    with args.extension.open("ab") as extension:
        extension.write(custom_section)


if __name__ == "__main__":
    main()
