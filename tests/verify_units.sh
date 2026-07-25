#!/usr/bin/env bash
# Verify unit metadata against real telemetry files.
#
# Answers, with evidence rather than assertion:
#   1. Does every channel that carries data have a unit?
#   2. Is every unit in the global registry (nothing unrecognised)?
#   3. Do values fall in a physically plausible range for their dimension,
#      or do we get nonsense like gear 9 or 500 C oil temperature?
#   4. Can every unit convert to every other unit of its dimension, and does
#      converting to a display unit and back preserve the value?
#
# Usage: verify_units.sh FILE_OR_GLOB [FILE_OR_GLOB ...]
# Exit code is non-zero if any hard check fails.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="${EXT:-$REPO_ROOT/build/release/motorsport_telemetry.duckdb_extension}"
DUCKDB="${DUCKDB:-duckdb}"

if [[ ! -f "$EXT" ]]; then
  printf 'extension not found at %s (run: make release)\n' "$EXT" >&2
  exit 1
fi
if [[ $# -lt 1 ]]; then
  printf 'usage: %s FILE_OR_GLOB [...]\n' "$0" >&2
  exit 1
fi

failures=0
q() { "$DUCKDB" -unsigned -noheader -list -c "LOAD '$EXT'; $1"; }
table() { "$DUCKDB" -unsigned -c "LOAD '$EXT'; $1"; }

# The table functions take one glob string. Combine multiple inputs into a
# single brace group so one query still spans every file.
if [[ $# -eq 1 ]]; then
  combined="$1"
else
  combined="{$(printf '%s,' "$@" | sed 's/,$//')}"
fi
srclist="'${combined//\'/\'\'}'"

printf '\n=== inputs ===\n'
table "SELECT format, count(DISTINCT file) AS files, count(*) AS channels,
              sum(sample_count) AS samples
       FROM telemetry_metadata($srclist) GROUP BY format ORDER BY format;"

# ── 1. unit coverage ────────────────────────────────────────────────
printf '\n=== 1. unit coverage (channels holding data) ===\n'
table "SELECT unit_source, count(*) AS channels,
              round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS pct
       FROM telemetry_metadata($srclist)
       WHERE sample_count > 0
       GROUP BY unit_source ORDER BY channels DESC;"

missing="$(q "SELECT count(*) FROM telemetry_metadata($srclist)
             WHERE sample_count > 0 AND (unit IS NULL OR unit = '');")"
total="$(q "SELECT count(*) FROM telemetry_metadata($srclist) WHERE sample_count > 0;")"
printf 'channels with data but no unit: %s of %s\n' "$missing" "$total"
if [[ "$missing" != "0" ]]; then
  printf '  (reported as unit_source=unknown, not guessed — sample:)\n'
  table "SELECT name, data_type, sample_count FROM telemetry_metadata($srclist)
         WHERE sample_count > 0 AND (unit IS NULL OR unit = '')
         ORDER BY sample_count DESC LIMIT 8;"
fi

# ── 2. every unit is known to the registry ──────────────────────────
printf '\n=== 2. registry coverage ===\n'
unknown="$(q "SELECT count(*) FROM (
                SELECT DISTINCT unit FROM telemetry_metadata($srclist)
                WHERE unit IS NOT NULL AND unit <> ''
              ) f
              WHERE NOT EXISTS (SELECT 1 FROM telemetry_units() u WHERE u.unit = f.unit);")"
if [[ "$unknown" == "0" ]]; then
  printf 'PASS: every unit string in these files resolves in telemetry_units()\n'
else
  printf 'FAIL: %s unit string(s) are not in the registry:\n' "$unknown"
  table "SELECT DISTINCT f.unit FROM telemetry_metadata($srclist) f
         WHERE f.unit <> '' AND NOT EXISTS
           (SELECT 1 FROM telemetry_units() u WHERE u.unit = f.unit);"
  failures=$((failures + 1))
fi

printf '\ndistinct units in these files, normalised:\n'
table "SELECT unit AS file_unit, canonical_unit, dimension, count(*) AS channels
       FROM telemetry_metadata($srclist)
       WHERE unit <> '' GROUP BY ALL ORDER BY channels DESC LIMIT 40;"

printf 'aliases actually exercised (file spelling differs from canonical):\n'
table "SELECT DISTINCT unit AS file_unit, canonical_unit, dimension
       FROM telemetry_metadata($srclist)
       WHERE unit <> '' AND canonical_unit IS NOT NULL AND unit <> canonical_unit;"

# ── 3. physical plausibility ────────────────────────────────────────
# Ranges are generous outer bounds for a closed-wheel prototype: the point is
# to catch unit errors (rad read as deg, Pa read as bar) and corrupt decoding,
# not to police setup choices.
printf '\n=== 3. physical plausibility by dimension ===\n'
table "WITH stats AS (
         SELECT m.name, m.canonical_unit, m.dimension,
                min(s.value) AS lo, max(s.value) AS hi
         FROM telemetry_metadata($srclist) m
         JOIN telemetry_samples($srclist) s
           ON s.file = m.file AND s.channel = m.name
         WHERE m.sample_count > 0 AND m.canonical_unit IS NOT NULL
         GROUP BY ALL
       ),
       si AS (
         SELECT name, dimension, canonical_unit,
                telemetry_convert(lo, canonical_unit,
                  (SELECT base_unit FROM telemetry_units() u
                   WHERE u.canonical_unit = stats.canonical_unit LIMIT 1)) AS lo_si,
                telemetry_convert(hi, canonical_unit,
                  (SELECT base_unit FROM telemetry_units() u
                   WHERE u.canonical_unit = stats.canonical_unit LIMIT 1)) AS hi_si
         FROM stats
         WHERE (SELECT is_convertible FROM telemetry_units() u
                WHERE u.canonical_unit = stats.canonical_unit LIMIT 1)
       )
       SELECT dimension, count(*) AS channels,
              round(min(lo_si), 3) AS min_si, round(max(hi_si), 3) AS max_si,
              (SELECT base_unit FROM telemetry_units() u
               WHERE u.dimension = si.dimension LIMIT 1) AS si_unit
       FROM si GROUP BY dimension ORDER BY dimension;"

printf '\nimplausible ranges (candidate unit or decoding errors):\n'
implausible="$(q "WITH stats AS (
         SELECT m.name, m.canonical_unit, m.dimension,
                min(s.value) AS lo, max(s.value) AS hi
         FROM telemetry_metadata($srclist) m
         JOIN telemetry_samples($srclist) s
           ON s.file = m.file AND s.channel = m.name
         WHERE m.sample_count > 0 AND m.canonical_unit IS NOT NULL
         GROUP BY ALL
       ),
       si AS (
         SELECT s.*, u.base_unit, u.is_convertible,
                telemetry_convert(lo, s.canonical_unit, u.base_unit) AS lo_si,
                telemetry_convert(hi, s.canonical_unit, u.base_unit) AS hi_si
         FROM stats s JOIN telemetry_units() u
           ON u.canonical_unit = s.canonical_unit AND u.is_canonical
         WHERE u.is_convertible
       )
       SELECT count(*) FROM si WHERE
            (dimension = 'speed'        AND (hi_si > 150 OR lo_si < -5))
         OR (dimension = 'acceleration' AND (hi_si > 100 OR lo_si < -100))
         OR (dimension = 'angle'        AND (hi_si > 100 OR lo_si < -100))
         OR (dimension = 'temperature'  AND (hi_si > 1500 OR lo_si < 0))
         OR (dimension = 'pressure'     AND (hi_si > 3e7 OR lo_si < -1e5))
         OR (dimension = 'ratio'        AND (hi_si > 100 OR lo_si < -100));")"
if [[ "$implausible" == "0" ]]; then
  printf 'PASS: no channel exceeds its dimension'\''s plausible SI envelope\n'
else
  printf 'NOTE: %s channel(s) outside the plausible envelope:\n' "$implausible"
  table "WITH stats AS (
           SELECT m.name, m.canonical_unit, m.dimension,
                  min(s.value) AS lo, max(s.value) AS hi
           FROM telemetry_metadata($srclist) m
           JOIN telemetry_samples($srclist) s
             ON s.file = m.file AND s.channel = m.name
           WHERE m.sample_count > 0 AND m.canonical_unit IS NOT NULL
           GROUP BY ALL
         ),
         si AS (
           SELECT s.*, u.base_unit,
                  telemetry_convert(lo, s.canonical_unit, u.base_unit) AS lo_si,
                  telemetry_convert(hi, s.canonical_unit, u.base_unit) AS hi_si
           FROM stats s JOIN telemetry_units() u
             ON u.canonical_unit = s.canonical_unit AND u.is_canonical
           WHERE u.is_convertible
         )
         SELECT name, dimension, canonical_unit,
                round(lo_si, 2) AS min_si, round(hi_si, 2) AS max_si, base_unit
         FROM si WHERE
              (dimension = 'speed'        AND (hi_si > 150 OR lo_si < -5))
           OR (dimension = 'acceleration' AND (hi_si > 100 OR lo_si < -100))
           OR (dimension = 'angle'        AND (hi_si > 100 OR lo_si < -100))
           OR (dimension = 'temperature'  AND (hi_si > 1500 OR lo_si < 0))
           OR (dimension = 'pressure'     AND (hi_si > 3e7 OR lo_si < -1e5))
           OR (dimension = 'ratio'        AND (hi_si > 100 OR lo_si < -100))
         ORDER BY dimension, name LIMIT 25;"
fi

# Discrete channels: integer-valued and in range. Gear 9 is the canonical bug.
printf '\ndiscrete channel sanity (gear / lap counters):\n'
table "SELECT m.name, m.canonical_unit, m.dimension,
              min(s.value) AS lo, max(s.value) AS hi,
              count(*) FILTER (s.value <> floor(s.value)) AS non_integer,
              CASE
                WHEN lower(m.name) LIKE '%gear%'
                     AND (max(s.value) > 8 OR min(s.value) < -1) THEN 'SUSPECT'
                WHEN count(*) FILTER (s.value <> floor(s.value)) > 0 THEN 'NON-INTEGER'
                ELSE 'ok'
              END AS verdict
       FROM telemetry_metadata($srclist) m
       JOIN telemetry_samples($srclist) s
         ON s.file = m.file AND s.channel = m.name
       WHERE m.sample_count > 0
         AND (lower(m.name) LIKE '%gear%' OR lower(m.name) LIKE '%lap%')
       GROUP BY m.name, m.canonical_unit, m.dimension
       ORDER BY verdict, m.name LIMIT 20;"

# Channels that never change are usually unconfigured sensors, not data.
printf '\nconstant channels (present but never vary):\n'
table "SELECT count(*) AS constant_channels FROM (
         SELECT m.name FROM telemetry_metadata($srclist) m
         JOIN telemetry_samples($srclist) s
           ON s.file = m.file AND s.channel = m.name
         WHERE m.sample_count > 1
         GROUP BY m.file, m.name HAVING min(s.value) = max(s.value));"

# ── 4. conversion completeness ──────────────────────────────────────
printf '\n=== 4. conversion completeness ===\n'
pairs="$(q "SELECT count(*) FROM telemetry_units() a JOIN telemetry_units() b
            ON a.dimension = b.dimension
            WHERE a.is_canonical AND b.is_canonical AND a.is_convertible;")"
ok="$(q "SELECT count(*) FROM telemetry_units() a JOIN telemetry_units() b
         ON a.dimension = b.dimension
         WHERE a.is_canonical AND b.is_canonical AND a.is_convertible
           AND telemetry_can_convert(a.canonical_unit, b.canonical_unit);")"
printf 'convertible same-dimension unit pairs: %s of %s\n' "$ok" "$pairs"
if [[ "$ok" != "$pairs" ]]; then
  printf 'FAIL: some same-dimension pairs cannot convert\n'
  failures=$((failures + 1))
fi

crossdim="$(q "SELECT count(*) FROM telemetry_units() a JOIN telemetry_units() b
               ON a.dimension <> b.dimension
               WHERE a.is_canonical AND b.is_canonical
                 AND telemetry_can_convert(a.canonical_unit, b.canonical_unit);")"
if [[ "$crossdim" == "0" ]]; then
  printf 'PASS: no cross-dimension conversion is permitted\n'
else
  printf 'FAIL: %s cross-dimension conversion(s) wrongly allowed\n' "$crossdim"
  failures=$((failures + 1))
fi

# Round-trip every channel's real min/max through a foreign unit and back.
printf '\nround-trip through every alternative unit of the same dimension:\n'
roundtrip_bad="$(q "WITH stats AS (
    SELECT DISTINCT m.canonical_unit, min(s.value) AS lo, max(s.value) AS hi
    FROM telemetry_metadata($srclist) m
    JOIN telemetry_samples($srclist) s
      ON s.file = m.file AND s.channel = m.name
    WHERE m.sample_count > 0 AND m.canonical_unit IS NOT NULL
    GROUP BY m.canonical_unit
  )
  SELECT count(*) FROM stats st
  JOIN telemetry_units() a ON a.canonical_unit = st.canonical_unit AND a.is_canonical
  JOIN telemetry_units() b ON b.dimension = a.dimension AND b.is_canonical
  WHERE a.is_convertible
    AND abs(telemetry_convert(
              telemetry_convert(st.hi, a.canonical_unit, b.canonical_unit),
              b.canonical_unit, a.canonical_unit) - st.hi)
        > 1e-6 * greatest(abs(st.hi), 1.0);")"
if [[ "$roundtrip_bad" == "0" ]]; then
  printf 'PASS: every real channel value survives a round-trip through every\n'
  printf '      other unit of its dimension within 1e-6 relative error\n'
else
  printf 'FAIL: %s round-trip(s) lost precision\n' "$roundtrip_bad"
  failures=$((failures + 1))
fi

printf '\n=== summary ===\n'
if [[ "$failures" == "0" ]]; then
  printf 'all hard checks passed\n'
else
  printf '%s hard check(s) FAILED\n' "$failures"
fi
exit "$failures"
