#!/usr/bin/env python3
"""Exercise duckdb-pds and print useful metadata, raw, and resampled stats.

This intentionally uses the DuckDB CLI instead of the optional Python duckdb
package, so it has no Python dependencies beyond the standard library.
"""

from __future__ import annotations

import argparse
import csv
import io
import shutil
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterable

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_EXTENSION = ROOT / "build" / "release" / "pds.duckdb_extension"


def sql_string(value: str | Path) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def sql_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def query(duckdb: str, extension: Path, sql: str) -> list[dict[str, str]]:
    command = [
        duckdb,
        "-unsigned",
        "-csv",
        "-header",
        "-c",
        f"LOAD {sql_string(extension)}; {sql}",
    ]
    result = subprocess.run(command, text=True, capture_output=True)
    if result.returncode != 0:
        message = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(message)
    return list(csv.DictReader(io.StringIO(result.stdout)))


def display_file(value: str) -> str:
    path = Path(value)
    return path.name or value


def number(value: str) -> str:
    if value in ("", "NULL", None):
        return "-"
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return str(value)
    magnitude = abs(parsed)
    if magnitude >= 1e9 or (magnitude != 0 and magnitude < 1e-4):
        return f"{parsed:.5e}"
    if magnitude >= 1000:
        return f"{parsed:,.1f}"
    return f"{parsed:.5g}"


def print_table(title: str, columns: list[tuple[str, str]], rows: Iterable[dict[str, str]]) -> None:
    materialized = list(rows)
    print(f"\n{title}")
    if not materialized:
        print("  (none)")
        return
    rendered: list[list[str]] = []
    for row in materialized:
        rendered.append([str(row.get(key, "")) for key, _ in columns])
    widths = [len(label) for _, label in columns]
    for row in rendered:
        for i, value in enumerate(row):
            widths[i] = min(60, max(widths[i], len(value)))
    print("  " + "  ".join(label.ljust(widths[i]) for i, (_, label) in enumerate(columns)))
    print("  " + "  ".join("-" * width for width in widths))
    for row in rendered:
        print("  " + "  ".join(value[: widths[i]].ljust(widths[i]) for i, value in enumerate(row)))


def normalized(name: str) -> str:
    return "".join(char for char in name.lower() if char.isalnum())


COMMON_CHANNELS: list[tuple[str, list[str]]] = [
    ("speed", ["speedref", "corrspeed", "groundspeed", "vehrefspeed", "wheelspeedavg", "uspeed"]),
    ("throttle", ["driverthrottlepos", "accelpedalpos", "fbwdrivertps", "pps", "tpsreal"]),
    ("front brake", ["brakepressuref", "brakepressurefr", "pfbrake"]),
    ("rpm", ["enginespeed", "rpm", "enginerpm"]),
    ("steering", ["steeringangle", "steerangle", "steering"]),
    ("gear", ["gearpos", "gearposdisplay", "gear"]),
    ("longitudinal accel", ["iaccellong", "gforcelong", "fiaaccelx", "glong"]),
    ("lateral accel", ["iaccellat", "gforcelat", "fiaaccely", "glat"]),
]


def choose_channels(metadata: list[dict[str, str]], requested: str | None) -> list[str]:
    sampled_names: dict[str, str] = {}
    for row in metadata:
        if int(row["sample_count"]) > 0:
            sampled_names.setdefault(row["name"].lower(), row["name"])

    if requested:
        result: list[str] = []
        missing: list[str] = []
        for name in (part.strip() for part in requested.split(",")):
            if not name:
                continue
            actual = sampled_names.get(name.lower())
            if actual is None:
                missing.append(name)
            elif actual not in result:
                result.append(actual)
        if missing:
            raise RuntimeError(f"sampled channel(s) not found: {', '.join(missing)}")
        return result

    by_normalized: dict[str, str] = {}
    for actual in sampled_names.values():
        by_normalized.setdefault(normalized(actual), actual)
    selected: list[str] = []
    for _, priorities in COMMON_CHANNELS:
        match = next((by_normalized[name] for name in priorities if name in by_normalized), None)
        if match and match not in selected:
            selected.append(match)
    if not selected:
        selected.extend(list(sampled_names.values())[:4])
    return selected


def metadata_summary(metadata: list[dict[str, str]]) -> None:
    by_file: dict[str, list[dict[str, str]]] = defaultdict(list)
    for row in metadata:
        by_file[row["file"]].append(row)

    file_rows: list[dict[str, str]] = []
    for filename, rows in sorted(by_file.items()):
        sampled = [row for row in rows if int(row["sample_count"]) > 0]
        raw_samples = sum(int(row["sample_count"]) for row in sampled)
        duration = max((int(row["duration_ns"]) for row in sampled), default=0) / 1e9
        try:
            size = Path(filename).stat().st_size
        except OSError:
            size = 0
        file_rows.append(
            {
                "file": display_file(filename),
                "size": f"{size / 1024 / 1024:.1f}",
                "definitions": str(len(rows)),
                "sampled": str(len(sampled)),
                "samples": f"{raw_samples:,}",
                "duration": f"{duration:.3f}",
            }
        )
    print_table(
        "Files",
        [("file", "file"), ("size", "MiB"), ("definitions", "definitions"),
         ("sampled", "sampled"), ("samples", "raw samples"), ("duration", "max seconds")],
        file_rows,
    )

    rates: Counter[float] = Counter()
    rate_samples: Counter[float] = Counter()
    for row in metadata:
        if int(row["sample_count"]) == 0 or not row["frequency_hz"]:
            continue
        rate = float(row["frequency_hz"])
        rates[rate] += 1
        rate_samples[rate] += int(row["sample_count"])
    rate_rows = [
        {"rate": number(str(rate)), "channels": str(rates[rate]), "samples": f"{rate_samples[rate]:,}"}
        for rate in sorted(rates)
    ]
    print_table(
        "Native sample rates",
        [("rate", "Hz"), ("channels", "channel definitions"), ("samples", "raw samples")],
        rate_rows,
    )


def raw_stats(duckdb: str, extension: Path, pattern: str, channels: list[str]) -> list[dict[str, str]]:
    selected = ",".join(channels)
    rows = query(
        duckdb,
        extension,
        f"""
        SELECT file, channel, any_value(unit) AS unit,
               any_value(frequency_hz) AS frequency_hz,
               count(*) AS samples,
               min(value) FILTER (WHERE isfinite(value)) AS min,
               avg(value) FILTER (WHERE isfinite(value)) AS mean,
               max(value) FILTER (WHERE isfinite(value)) AS max
        FROM pds_samples({sql_string(pattern)}, channel := {sql_string(selected)})
        GROUP BY file, channel
        ORDER BY file, channel
        """,
    )
    for row in rows:
        row["file"] = display_file(row["file"])
        for key in ("frequency_hz", "min", "mean", "max"):
            row[key] = number(row[key])
        row["samples"] = f"{int(row['samples']):,}"
    return rows


def interpolated_stats(
    duckdb: str, extension: Path, pattern: str, channels: list[str], rate: int
) -> list[dict[str, str]]:
    selected = ",".join(channels)
    branches = []
    for channel in channels:
        identifier = sql_identifier(channel)
        branches.append(
            f"""
            SELECT filename, {sql_string(channel)} AS channel, count(*) AS rows,
                   count({identifier}) FILTER (WHERE isfinite({identifier})) AS finite,
                   min({identifier}) FILTER (WHERE isfinite({identifier})) AS min,
                   avg({identifier}) FILTER (WHERE isfinite({identifier})) AS mean,
                   max({identifier}) FILTER (WHERE isfinite({identifier})) AS max
            FROM telemetry GROUP BY filename
            """
        )
    rows = query(
        duckdb,
        extension,
        f"""
        WITH telemetry AS MATERIALIZED (
            SELECT * FROM read_pds(
                {sql_string(pattern)}, rate := {rate}, channels := {sql_string(selected)},
                interpolate := 'linear', filename := true
            )
        )
        {" UNION ALL ".join(branches)}
        ORDER BY filename, channel
        """,
    )
    for row in rows:
        row["filename"] = display_file(row["filename"])
        for key in ("min", "mean", "max"):
            row[key] = number(row[key])
        for key in ("rows", "finite"):
            row[key] = f"{int(row[key]):,}"
    return rows


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Print common metadata, raw-sample, and interpolated stats for PDS files."
    )
    parser.add_argument("path", help="PDS path or quoted glob, e.g. '**/Offloaded/*.pds'")
    parser.add_argument("--extension", type=Path, default=DEFAULT_EXTENSION)
    parser.add_argument("--duckdb", default="duckdb", help="DuckDB CLI executable")
    parser.add_argument("--rate", type=int, default=100, help="wide resampling rate in Hz")
    parser.add_argument(
        "--channels",
        help="comma-separated exact channel names; defaults to common driving channels",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not 1 <= args.rate <= 5000:
        print("error: --rate must be between 1 and 5000", file=sys.stderr)
        return 2
    if not args.extension.is_file():
        print(f"error: extension not found: {args.extension}\nRun `make build` first.", file=sys.stderr)
        return 2
    if shutil.which(args.duckdb) is None:
        print(f"error: DuckDB CLI not found: {args.duckdb}", file=sys.stderr)
        return 2

    try:
        metadata = query(
            args.duckdb,
            args.extension.resolve(),
            f"SELECT * FROM pds_metadata({sql_string(args.path)}) ORDER BY file, channel_id",
        )
        channels = choose_channels(metadata, args.channels)
        metadata_summary(metadata)
        print(f"\nSelected common channels: {', '.join(channels)}")
        print_table(
            "Raw mixed-rate sample stats",
            [("file", "file"), ("channel", "channel"), ("unit", "unit"),
             ("frequency_hz", "Hz"), ("samples", "samples"), ("min", "min"),
             ("mean", "mean"), ("max", "max")],
            raw_stats(args.duckdb, args.extension.resolve(), args.path, channels),
        )
        print_table(
            f"Interpolated wide stats at {args.rate} Hz",
            [("filename", "file"), ("channel", "channel"), ("rows", "rows"),
             ("finite", "finite"), ("min", "min"), ("mean", "mean"), ("max", "max")],
            interpolated_stats(args.duckdb, args.extension.resolve(), args.path, channels, args.rate),
        )
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
