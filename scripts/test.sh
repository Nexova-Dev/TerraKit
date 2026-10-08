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

  # Registry and `terrakit add`. Needs a release binary and the GeoNames source cache, so it
  # runs only when both are present and says so rather than skipping silently.
  RELEASE_BIN=cli/target/release/terrakit
  if [[ -x "$RELEASE_BIN" ]] && [[ -n "${TERRAKIT_CACHE:-}" ]] && \
     [[ -d "${TERRAKIT_CACHE}" ]]; then
    cargo build --manifest-path cli/Cargo.toml --release --quiet
    run_suite "registry install and refusals" \
      bash scripts/test_registry.sh
  else
    printf '\n\033[1m=== registry install and refusals\033[0m\n'
    printf '  SKIPPED: need a release binary and TERRAKIT_CACHE\n'
    printf '  cargo build --manifest-path cli/Cargo.toml --release\n'
    printf '  TERRAKIT_CACHE=... bash scripts/test_registry.sh\n'
  fi
else
  printf '\n\033[1m=== Rust CLI\033[0m\n  SKIPPED: cargo not found\n'
fi

# The Swift SDK. Two halves, because they have different prerequisites:
#
#   * the normalization suite needs nothing but the source tree, so it always runs. It is what
#     proves the SDK compiles and agrees with `core/spec/normalization.md` on every push.
#   * the cross-SDK battery needs built packages, so it runs only when some are handed to it and
#     says so otherwise.
#
# `swift` is absent on a Linux box without a toolchain; skipping is stated, never silent.
if command -v swift >/dev/null 2>&1; then
  # The SQLite3 module is part of Apple's toolchain and is not guaranteed elsewhere. Checked, because
  # the failure otherwise arrives as a compile error in the module that says nothing about why.
  #
  # `bash -c` rather than `env -C`: `env -C` is GNU/FreeBSD and is not present on every macOS
  # this has to run on, and a test runner that fails because of a flag difference is worse than
  # one extra shell.
  if swift -e 'import SQLite3' >/dev/null 2>&1; then
    run_suite "Swift SDK (normalization conformance)" \
      bash -c 'cd sdk/swift && swift test --filter Normalization'

  # The cross-SDK battery needs built packages. It is run for **both** SDKs, and separately, because
  # each is held to a different oracle: Swift against the CLI, Kotlin against the CLI *and* Swift.
  # A suite that ran only one would leave the other unverified while looking complete.
  if [[ -n "${TERRAKIT_CONFORMANCE_PACKAGES:-}" ]] && \
     [[ -d "${TERRAKIT_CONFORMANCE_PACKAGES}" ]]; then
    run_suite "Swift SDK (cross-SDK conformance)" \
      bash -c "cd sdk/swift && \
        TERRAKIT_CONFORMANCE_PACKAGES='$TERRAKIT_CONFORMANCE_PACKAGES' swift test"
  else
      printf '\n\033[1m=== Swift SDK (cross-SDK conformance)\033[0m\n'
      printf '  SKIPPED: need built packages\n'
      printf '  TERRAKIT_CONFORMANCE_PACKAGES=<datasets> bash scripts/test.sh\n'
    fi
  else
    printf '\n\033[1m=== Swift SDK\033[0m\n'
    printf '  SKIPPED: this Swift toolchain has no SQLite3 module\n'
    printf '  The Swift SDK targets Apple platforms; see scripts/test.sh and CI job `sdks`.\n'
  fi
else
  printf '\n\033[1m=== Swift SDK\033[0m\n'
  printf '  SKIPPED: swift not found (macOS ships it; Linux needs a toolchain)\n'
fi

# The Kotlin SDK. The Gradle wrapper is committed, so this needs no local Gradle -- only a JDK.
#
# `jvmTest` is invoked rather than `check` so the Android variant is not built: it needs an SDK, and
# ADR-008 makes it conditional for exactly that reason.
if command -v java >/dev/null 2>&1; then
  run_suite "Kotlin SDK (normalization conformance)" \
    bash -c 'cd sdk/kotlin && ./gradlew jvmTest --tests "*Normalization*" --quiet'

  if [[ -n "${TERRAKIT_CONFORMANCE_PACKAGES:-}" ]] && \
     [[ -d "${TERRAKIT_CONFORMANCE_PACKAGES}" ]]; then
    run_suite "Kotlin SDK (cross-SDK conformance)" \
      bash -c "cd sdk/kotlin && \
        TERRAKIT_CONFORMANCE_PACKAGES='$TERRAKIT_CONFORMANCE_PACKAGES' \
        TERRAKIT_REPO_ROOT='$REPO_ROOT' ./gradlew crossSdkConformance --quiet"
  else
    printf '\n\033[1m=== Kotlin SDK (cross-SDK conformance)\033[0m\n'
    printf '  SKIPPED: need built packages\n'
    printf '  TERRAKIT_CONFORMANCE_PACKAGES=<datasets> bash scripts/test.sh\n'
  fi
else
  printf '\n\033[1m=== Kotlin SDK\033[0m\n'
  printf '  SKIPPED: java not found\n'
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
