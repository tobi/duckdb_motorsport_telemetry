#!/usr/bin/env python3
"""Synchronize generated package metadata from VERSION."""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_FILE = ROOT / "VERSION"
WORKSPACE_PACKAGES = {
    "duckdb-motorsport-telemetry",
}


def read_version() -> tuple[str, str]:
    extension_version = VERSION_FILE.read_text(encoding="utf-8").strip()
    match = re.fullmatch(r"v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)", extension_version)
    if not match:
        raise SystemExit(
            f"{VERSION_FILE}: expected vMAJOR.MINOR.PATCH, got {extension_version!r}"
        )
    return extension_version, match.group(1)


def replace(path: Path, pattern: str, replacement: str, *, count: int = 0) -> None:
    original = path.read_text(encoding="utf-8")
    updated, matches = re.subn(pattern, replacement, original, count=count, flags=re.MULTILINE)
    if matches == 0:
        raise SystemExit(f"{path}: version pattern not found")
    path.write_text(updated, encoding="utf-8")


def expected_files(extension_version: str, cargo_version: str) -> dict[Path, str]:
    paths = [
        ROOT / "Cargo.toml",
        ROOT / "Cargo.lock",
        ROOT / "wasm-extension" / "Cargo.toml",
        ROOT / "wasm-extension" / "Cargo.lock",
        ROOT / "web" / "package.json",
        ROOT / "community-extension" / "description.yml",
    ]
    files = {path: path.read_text(encoding="utf-8") for path in paths}

    files[ROOT / "Cargo.toml"] = re.sub(
        r'(?m)^(version = ")[^"]+("$)', rf"\g<1>{cargo_version}\g<2>", files[ROOT / "Cargo.toml"], count=1
    )
    for path in (ROOT / "Cargo.lock", ROOT / "wasm-extension" / "Cargo.lock"):
        content = files[path]
        for package in WORKSPACE_PACKAGES | {"duckdb-motorsport-telemetry-wasm"}:
            content = re.sub(
                rf'(\[\[package\]\]\nname = "{re.escape(package)}"\nversion = ")[^"]+("$)',
                rf"\g<1>{cargo_version}\g<2>",
                content,
                count=1,
                flags=re.MULTILINE,
            )
        files[path] = content

    files[ROOT / "wasm-extension" / "Cargo.toml"] = re.sub(
        r'(?m)^(version = ")[^"]+("$)', rf"\g<1>{cargo_version}\g<2>", files[ROOT / "wasm-extension" / "Cargo.toml"], count=1
    )
    files[ROOT / "web" / "package.json"] = re.sub(
        r'("version": ")[^"]+(",$)', rf"\g<1>{cargo_version}\g<2>", files[ROOT / "web" / "package.json"], count=1, flags=re.MULTILINE
    )
    files[ROOT / "community-extension" / "description.yml"] = re.sub(
        r'(?m)^(  version: ).*$', rf"\g<1>{cargo_version}", files[ROOT / "community-extension" / "description.yml"], count=1
    )
    return files


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="fail when generated files are stale")
    args = parser.parse_args()

    extension_version, cargo_version = read_version()
    expected = expected_files(extension_version, cargo_version)
    stale = [path for path, content in expected.items() if path.read_text(encoding="utf-8") != content]
    if args.check:
        if stale:
            for path in stale:
                print(f"stale: {path.relative_to(ROOT)}", file=sys.stderr)
            return 1
        return 0

    for path, content in expected.items():
        if path.read_text(encoding="utf-8") != content:
            path.write_text(content, encoding="utf-8")
    print(f"synchronized package metadata to {extension_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
