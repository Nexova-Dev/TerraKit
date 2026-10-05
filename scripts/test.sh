#!/usr/bin/env bash
#
# Test runner for everything that needs no network and no Rust toolchain.
#
# Run from the repository root:
#   scripts/test.sh
#
# Deliberately plain: no test framework. These suites are assertions over
# geographic data and schema invariants, and a dependency that can break the
# build is a worse trade than a file with functions in it.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

FAILED=()
PASSED=()

run_suite() {
  local name="$1"; shift
  printf '\n\033[1m=== %s\033[0m\n' "$name"
  if "$@"; then
    PASSED+=("$name")
  else
    FAILED+=("$name")
  fi
}

run_suite "normalization conformance (spec oracle)" \
  python3 tests/conformance/run_normalization.py

run_suite "phonetic conformance" \
  python3 tests/conformance/test_phonetic.py

run_suite "pipeline unit tests" \
  python3 data-pipeline/tests/test_pipeline.py

run_suite "name ingestion" \
  python3 data-pipeline/tests/test_names.py

run_suite "package schema smoke test" \
  bash scripts/test_schema.sh

run_suite "internal documentation links" \
  python3 tools/check_links.py

# Rust CLI. Skipped rather than failed when cargo is absent, so the suite still
# runs on a machine without a Rust toolchain — but never silently: the skip is
# printed.
if command -v cargo > /dev/null 2>&1; then
  run_suite "Rust CLI (unit + normalization conformance)" \
    cargo test --manifest-path cli/Cargo.toml --quiet
else
  printf '\n\033[1m=== Rust CLI\033[0m\n  SKIPPED: cargo not found\n'
fi

printf '\n\033[1m=== summary\033[0m\n'
for suite in "${PASSED[@]:-}"; do
  [[ -n "$suite" ]] && printf '  \033[32mPASS\033[0m %s\n' "$suite"
done
for suite in "${FAILED[@]:-}"; do
  [[ -n "$suite" ]] && printf '  \033[31mFAIL\033[0m %s\n' "$suite"
done

if [[ ${#FAILED[@]} -gt 0 ]]; then
  printf '\n%d suite(s) failed\n' "${#FAILED[@]}"
  exit 1
fi

printf '\nall suites passed\n'
