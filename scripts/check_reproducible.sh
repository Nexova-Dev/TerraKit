#!/usr/bin/env bash
#
# Reproducibility check.
#
# PACKAGE_FORMAT.md section 12 and ARCHITECTURE.md section 11 make byte-identical
# output a testable property, not an aspiration: the same inputs must produce the
# same SHA-256. Without a test, "reproducible" is a claim.
#
# Builds the same country twice into separate directories and compares the
# package checksums and the manifests.
#
# Usage:
#   scripts/check_reproducible.sh TR [--level postal]
#
# Requires cached GeoNames sources; pass --download to fetch them.

set -euo pipefail

COUNTRY="${1:?usage: check_reproducible.sh <ISO2> [--level LEVEL] [--download]}"
shift || true

LEVEL="postal"
FETCH=0
CACHE_DIR="${TERRAKIT_CACHE:-$HOME/.cache/terrakit/sources}"
OUT_ROOT="$(mktemp -d)"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --level) LEVEL="$2"; shift 2 ;;
    --download) FETCH=1; shift ;;
    --cache) CACHE_DIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

trap 'rm -rf "$OUT_ROOT"' EXIT

DOWNLOAD_FLAG=()
if [[ $FETCH -eq 0 ]]; then
  DOWNLOAD_FLAG=(--skip-download)
fi

echo "Reproducibility check: $COUNTRY at level '$LEVEL'"
echo "  cache: $CACHE_DIR"
echo "  scratch: $OUT_ROOT"
echo

for RUN in 1 2; do
  echo "[run $RUN] building..."
  PYTHONPATH="$REPO_ROOT/data-pipeline/src" python3 -m terrakit_pipeline build \
    "$COUNTRY" \
    --level "$LEVEL" \
    --cache "$CACHE_DIR" \
    --out "$OUT_ROOT/run$RUN" \
    --repo-root "$REPO_ROOT" \
    "${DOWNLOAD_FLAG[@]}" \
    > "$OUT_ROOT/run$RUN.log" 2>&1 || {
      echo "run $RUN failed:" >&2
      tail -30 "$OUT_ROOT/run$RUN.log" >&2
      exit 1
    }
done

RUN1_DIR="$(find "$OUT_ROOT/run1" -maxdepth 1 -mindepth 1 -type d | head -1)"
RUN2_DIR="$(find "$OUT_ROOT/run2" -maxdepth 1 -mindepth 1 -type d | head -1)"

if [[ "$(basename "$RUN1_DIR")" != "$(basename "$RUN2_DIR")" ]]; then
  echo "FAIL: package versions differ between runs:" >&2
  echo "  $(basename "$RUN1_DIR")" >&2
  echo "  $(basename "$RUN2_DIR")" >&2
  exit 1
fi

SUM1="$(shasum -a 256 "$RUN1_DIR/data.sqlite" | awk '{print $1}')"
SUM2="$(shasum -a 256 "$RUN2_DIR/data.sqlite" | awk '{print $1}')"

echo
echo "  package version: $(basename "$RUN1_DIR")"
echo "  run 1 sha256:    $SUM1"
echo "  run 2 sha256:    $SUM2"

if [[ "$SUM1" != "$SUM2" ]]; then
  echo
  echo "FAIL: data.sqlite differs between runs." >&2
  echo "      Timestamps must come from SOURCE_DATE_EPOCH, row order must be" >&2
  echo "      fully determined, and SQLite layout must be fixed." >&2
  exit 1
fi

if ! diff -q "$RUN1_DIR/manifest.json" "$RUN2_DIR/manifest.json" > /dev/null; then
  echo
  echo "FAIL: manifest.json differs between runs:" >&2
  diff "$RUN1_DIR/manifest.json" "$RUN2_DIR/manifest.json" >&2 || true
  exit 1
fi

echo "  manifest:        identical"
echo
echo "PASS: reproducible. $(ls -lh "$RUN1_DIR/data.sqlite" | awk '{print $5}') package, byte-identical across runs."
