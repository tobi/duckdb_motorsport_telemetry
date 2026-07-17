#!/usr/bin/env python3
"""Package a native Rust cdylib as an unsigned DuckDB extension."""
from __future__ import annotations

import argparse
import shutil
from pathlib import Path

from append_metadata import field


def append_footer(path: Path, platform: str, api_version: str, version: str) -> None:
    section = b"\0" + bytes((147, 4, 16)) + b"duckdb_signature" + bytes((128, 4))
    fields = ["4", platform, api_version, version, "C_STRUCT", "", "", ""]
    section += b"".join(field(value) for value in reversed(fields)) + bytes(256)
    if len(section) != 534:
        raise AssertionError(len(section))
    with path.open("ab") as output:
        output.write(section)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--api-version", default="v1.2.0")
    parser.add_argument("--extension-version", default="v0.4.0")
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(args.library, args.output)
    append_footer(args.output, args.platform, args.api_version, args.extension_version)
    print(args.output)


if __name__ == "__main__":
    main()
