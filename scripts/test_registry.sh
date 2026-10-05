#!/usr/bin/env bash
#
# End-to-end test for the registry and `terrakit add`.
#
# Builds real packages with the pipeline, publishes them into a static registry, installs them
# with the compiled binary, and then attacks the result. The attacks are the point: a package
# manager's verification is not demonstrated by installing a good package, it is demonstrated by
# refusing the bad ones with the right exit code.
#
# Everything runs against `file://` URLs, so this needs no network and no credentials. That is
# also why the CLI supports `file://` as a first-class transport: an installer that cannot be
# tested offline cannot be tested at all.
#
# Usage:
#   scripts/test_registry.sh [--keep]
#
# Environment:
#   TERRAKIT_CACHE   source cache for the pipeline (see data-pipeline/sources/README.md)
#   PKGS             "TR GE" to test (default: TR, a small package)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

KEEP=0
[[ "${1:-}" == "--keep" ]] && KEEP=1

PKGS="${PKGS:-TR}"
CACHE="${TERRAKIT_CACHE:-$HOME/.cache/terrakit/sources}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/terrakit-registry-test.XXXXXX")"
BIN="$REPO_ROOT/cli/target/release/terrakit"

if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN does not exist. Run: cargo build --manifest-path cli/Cargo.toml --release" >&2
  exit 2
fi

pass=0
fail=0

green() { printf '\033[32m%s\033[0m' "$1"; }
red()   { printf '\033[31m%s\033[0m' "$1"; }

ok()   { printf '  %s %s\n' "$(green 'PASS')" "$1"; pass=$((pass + 1)); }
bad()  { printf '  %s %s\n' "$(red 'FAIL')" "$1"; fail=$((fail + 1)); }

# assert_exit <expected> <label> <command...>
assert_exit() {
  local expected="$1" label="$2"; shift 2
  set +e
  local output
  output="$("$@" 2>&1)"
  local actual=$?
  set -e
  if [[ "$actual" == "$expected" ]]; then
    ok "$label (exit $actual)"
  else
    bad "$label: expected exit $expected, got $actual"
    printf '%s\n' "$output" | sed 's/^/       /' | head -8
  fi
}

# assert_contains <needle> <label> <command...>
assert_contains() {
  local needle="$1" label="$2"; shift 2
  local output
  # Every caller here is checking a *refusal*, so a non-zero exit is the expected outcome and
  # must not trip `set -e`. The exit code itself is asserted separately by assert_exit.
  set +e
  output="$("$@" 2>&1)"
  set -e
  if grep -qF -- "$needle" <<< "$output"; then
    ok "$label"
  else
    bad "$label: output did not mention: $needle"
    printf '%s\n' "$output" | sed 's/^/       /' | head -8
  fi
}

assert_json() {
  local label="$1" payload="$2"
  if printf '%s' "$payload" | python3 -c 'import json,sys; json.load(sys.stdin)' 2>/dev/null; then
    ok "$label"
  else
    bad "$label: not valid JSON"
    printf '%s\n' "$payload" | head -3 | sed 's/^/       /'
  fi
}

cleanup() {
  if [[ "$KEEP" == "1" ]]; then
    printf '\nwork directory kept: %s\n' "$WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

printf '\033[1m=== building packages\033[0m\n'
for cc in $PKGS; do
  printf '  %s... ' "$cc"
  dir=$(PYTHONPATH=data-pipeline/src python3 -m terrakit_pipeline build "$cc" \
        --level postal --cache "$CACHE" --skip-download --out "$WORK/releases" \
        --repo-root "$REPO_ROOT" 2>/dev/null | grep -oE 'releases/[^ ]+' | tail -1 || true)
  # The pipeline prints a path; find the directory directly instead of parsing output.
  cc_lower=$(echo "$cc" | tr 'A-Z' 'a-z')
  dir=$(ls -d "$WORK/releases/terrakit-data-$cc_lower-"* 2>/dev/null | tail -1)
  [[ -n "$dir" ]] || { bad "build $cc produced no output"; exit 1; }

  PYTHONPATH=data-pipeline/src python3 -m terrakit_pipeline publish "$dir" \
    --sign-key keys/dev-ed25519.pem --sign-key-id terrakit-dev-2026 >/dev/null
  printf 'published %s\n' "$(basename "$dir")"
done

printf '\n\033[1m=== publishing the registry\033[0m\n'
python3 tools/build_registry.py "$WORK/releases" --out "$WORK/registry" \
  --base-url https://registry.invalid >/dev/null
REGISTRY="file://$WORK/registry"
ok "registry tree written"

# A fresh project directory, reused across cases.
new_project() {
  local name="$1"
  rm -rf "$WORK/$name"
  mkdir -p "$WORK/$name"
  (cd "$WORK/$name" && "$BIN" init >/dev/null)
}

printf '\n\033[1m=== installing\033[0m\n'
first_cc="$(echo "$PKGS" | cut -d' ' -f1)"
new_project ok-install
assert_exit 0 "install from a file registry" \
  bash -c "cd '$WORK/ok-install' && '$BIN' add $first_cc --registry '$REGISTRY' --cache-dir '$WORK/cache'"
assert_exit 0 "re-running add is idempotent" \
  bash -c "cd '$WORK/ok-install' && '$BIN' add $first_cc --registry '$REGISTRY' --cache-dir '$WORK/cache'"
assert_exit 0 "installed packages validate" \
  bash -c "cd '$WORK/ok-install' && '$BIN' validate"
assert_exit 0 "doctor passes" \
  bash -c "cd '$WORK/ok-install' && '$BIN' doctor"

# The registry tree must be position-independent: a copy elsewhere has to be read from the copy.
cp -R "$WORK/registry" "$WORK/registry-moved"
new_project moved
assert_exit 0 "a copied registry is read from the copy, not the original" \
  bash -c "cd '$WORK/moved' && '$BIN' add $first_cc --registry 'file://$WORK/registry-moved' --cache-dir '$WORK/cache2'"
assert_contains "registry-moved" "the copy is what --registry resolves against" \
  bash -c "cd '$WORK/moved' && '$BIN' add $first_cc --registry 'file://$WORK/registry-moved' --cache-dir '$WORK/cache2' 2>&1 | grep -o 'registry-moved' | head -1"

printf '\n\033[1m=== refusing tampered packages\033[0m\n'
CC_LOWER="$(echo "$first_cc" | tr 'A-Z' 'a-z')"
# The version directory NAME, not its path: the hostile copies below rebuild the same relative
# layout under a different registry root.
VERSION_SEGMENT="$(ls "$WORK/registry/packages/terrakit-data-$CC_LOWER" | head -1)"
if [[ -z "$VERSION_SEGMENT" ]]; then
  bad "no published version directory for $first_cc under $WORK/registry"
  exit 1
fi
VERSION_REL="packages/terrakit-data-$CC_LOWER/$VERSION_SEGMENT"

# 1. One flipped byte in the compressed artifact.
cp -R "$WORK/registry" "$WORK/reg-badbytes"
printf 'X' | dd "of=$WORK/reg-badbytes/$VERSION_REL/data.sqlite.zst" \
  bs=1 seek=1024 conv=notrunc 2>/dev/null
new_project badbytes
assert_exit 3 "a tampered artifact is refused (exit 3, not 1)" \
  bash -c "cd '$WORK/badbytes' && '$BIN' add $first_cc --registry 'file://$WORK/reg-badbytes' --cache-dir '$WORK/cache3'"
if [[ -d "$WORK/badbytes/.terrakit/packages/$first_cc" ]]; then
  bad "a refused package left something installed"
else
  ok "a refused package installs nothing"
fi

# 2. No signature at all.
cp -R "$WORK/registry" "$WORK/reg-unsigned"
rm -f "$WORK/reg-unsigned/$VERSION_REL/signature.json"
new_project unsigned
assert_exit 3 "an unsigned package is refused" \
  bash -c "cd '$WORK/unsigned' && '$BIN' add $first_cc --registry 'file://$WORK/reg-unsigned' --cache-dir '$WORK/cache4'"
assert_contains "no signature.json" "the refusal says the package was never signed" \
  bash -c "cd '$WORK/unsigned' && '$BIN' add $first_cc --registry 'file://$WORK/reg-unsigned' --cache-dir '$WORK/cache4'"

# 3. Manifest edited after signing.
cp -R "$WORK/registry" "$WORK/reg-forged"
python3 - "$WORK/reg-forged/$VERSION_REL/manifest.json" <<'PYEOF'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest["level"] = "full"
path.write_text(json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
PYEOF
new_project forged
assert_exit 3 "a manifest edited after signing is refused" \
  bash -c "cd '$WORK/forged' && '$BIN' add $first_cc --registry 'file://$WORK/reg-forged' --cache-dir '$WORK/cache5'"

# 4. Locally re-signed with a key nobody trusts.
cp -R "$WORK/registry" "$WORK/reg-rogue"
python3 - "$WORK/reg-rogue" "$VERSION_REL" <<'PYEOF'
import json, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

registry, relative = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
key = Ed25519PrivateKey.generate()
manifest = registry / relative / "manifest.json"
public = key.public_key().public_bytes(
    encoding=serialization.Encoding.Raw, format=serialization.PublicFormat.Raw
)
(registry / relative / "signature.json").write_text(json.dumps({
    "algorithm": "ed25519",
    "keyId": "rogue-key",
    "publicKey": public.hex(),
    "signature": key.sign(manifest.read_bytes()).hex(),
}, indent=2) + "\n")
PYEOF
new_project rogue
assert_exit 3 "a package signed by an untrusted key is refused" \
  bash -c "cd '$WORK/rogue' && '$BIN' add $first_cc --registry 'file://$WORK/reg-rogue' --cache-dir '$WORK/cache6'"
assert_contains "not trusted" "the refusal names the reason" \
  bash -c "cd '$WORK/rogue' && '$BIN' add $first_cc --registry 'file://$WORK/reg-rogue' --cache-dir '$WORK/cache6'"

printf '\n\033[1m=== publishing guards\033[0m\n'
# --production must refuse packages signed with the committed development key. Without this the
# guarantee is asserted in the documentation and tested nowhere, which is not a guarantee.
set +e
prod_output=$(python3 tools/build_registry.py "$WORK/releases" --out "$WORK/registry-prod" \
  --production 2>&1)
prod_code=$?
set -e
if [[ "$prod_code" != "0" ]]; then
  ok "--production refuses development-signed packages (exit $prod_code)"
else
  bad "--production accepted development-signed packages"
fi
if grep -qF 'development-signed' <<< "$prod_output"; then
  ok "--production names the reason"
else
  bad "--production did not explain the refusal"
  printf '%s\n' "$prod_output" | sed 's/^/       /' | head -5
fi
if [[ -d "$WORK/registry-prod/packages" ]]; then
  bad "--production wrote a registry tree before refusing"
else
  ok "--production publishes nothing when it refuses"
fi

# Rebuilding the same registry must be byte-identical, or a rebuild invalidates every CDN cache
# entry for no reason and readers see a different index each time.
python3 tools/build_registry.py "$WORK/releases" --out "$WORK/registry-again" \
  --base-url https://registry.invalid >/dev/null
if diff -q "$WORK/registry/index.json" "$WORK/registry-again/index.json" >/dev/null; then
  ok "the registry index is byte-identical across rebuilds"
else
  bad "the registry index changed between identical rebuilds"
fi

printf '\n\033[1m=== refusing bad requests\033[0m\n'
new_project requests
assert_exit 2 "a malformed country code is a usage error" \
  bash -c "cd '$WORK/requests' && '$BIN' add TURKEY --registry '$REGISTRY'"
assert_exit 2 "a malformed version range is a usage error" \
  bash -c "cd '$WORK/requests' && '$BIN' add $first_cc --version '2026.10.x' --registry '$REGISTRY'"
assert_exit 4 "an unsatisfiable pin reports not-found" \
  bash -c "cd '$WORK/requests' && '$BIN' add $first_cc --version '2099.1.0' --registry '$REGISTRY'"
assert_exit 4 "a country with no published package reports not-found" \
  bash -c "cd '$WORK/requests' && '$BIN' add ZW --registry '$REGISTRY'"
assert_exit 1 "--offline refuses to fetch rather than reporting not-found" \
  bash -c "cd '$WORK/requests' && '$BIN' add $first_cc --offline --registry '$REGISTRY'"

printf '\n\033[1m=== output contract\033[0m\n'
new_project output
payload=$(cd "$WORK/output" && "$BIN" add "$first_cc" --registry "$REGISTRY" \
  --cache-dir "$WORK/cache7" --json 2>/dev/null)
assert_json "add --json is valid JSON" "$payload"

new_project output2
for command in "list" "doctor" "info $first_cc"; do
  # shellcheck disable=SC2086
  payload=$(cd "$WORK/output2" && "$BIN" add "$first_cc" --registry "$REGISTRY" \
    --cache-dir "$WORK/cache8" >/dev/null 2>&1; cd "$WORK/output2" && "$BIN" $command --json 2>/dev/null)
  assert_json "$command --json is valid JSON" "$payload"
done

printf '\n\033[1m=== summary\033[0m\n'
printf '  %s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" == "0" ]] || exit 1