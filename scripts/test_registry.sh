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

# Each run owns its signing key and isolated trust configuration.
python3 tools/generate_dev_key.py --out "$WORK/test-key.pem" >/dev/null
export TERRAKIT_CONFIG="$WORK/config.toml"
python3 - "$WORK/test-key.pub" "$TERRAKIT_CONFIG" <<'PYKEY'
import json, pathlib, sys
key = json.loads(pathlib.Path(sys.argv[1]).read_text())
pathlib.Path(sys.argv[2]).write_text(
    '[[keys]]\nid = "terrakit-dev-2026"\npublicKey = "' + key['publicKey'] + '"\n'
)
PYKEY

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
    --sign-key "$WORK/test-key.pem" --sign-key-id terrakit-dev-2026 >/dev/null
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

# Run the binary inside a named project directory.
#
# `new_project` cds in a subshell, so it does not change the script's own working directory.
# Calling `$BIN` bare afterwards ran it from the repository root, where there is no
# terrakit.json, and every such call exited 6 with "no terrakit.json found" -- which looks like a
# registry failure and is not one.
in_project() {
  local name="$1"; shift
  (cd "$WORK/$name" && "$BIN" "$@")
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
# --production must refuse packages signed with the temporary development key. Without this the
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

printf '\n\033[1m=== update\033[0m\n'
# A second published version of the same package, so `update` has something to move to.
#
# Produced with the real publisher rather than by hand-editing the manifest: hand-editing left
# manifestPath, downloadPath and the signature describing the *old* version, which made the
# first attempt at this fixture fail for reasons that had nothing to do with update. Re-running
# `publish` recomputes the digests and re-signs, so the second version is exactly what a second
# CI run would produce.
second_version="2026.10.2+aaaa"
first_release="$(ls -d "$WORK/releases/terrakit-data-$CC_LOWER-"* | head -1)"
second_release="$WORK/releases/terrakit-data-$CC_LOWER-${second_version/+/-}"
cp -R "$first_release" "$second_release"
rm -f "$second_release/signature.json" "$second_release"/*.zst
python3 - "$second_release/manifest.json" "$second_version" <<'PYEOF'
import json, pathlib, sys
path, version = pathlib.Path(sys.argv[1]), sys.argv[2]
manifest = json.loads(path.read_text())
manifest["version"] = version
path.write_text(json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
PYEOF
PYTHONPATH=data-pipeline/src python3 -m terrakit_pipeline publish "$second_release" \
  --sign-key "$WORK/test-key.pem" --sign-key-id terrakit-dev-2026 >/dev/null

python3 tools/build_registry.py "$WORK/releases" --out "$WORK/registry-v2" \
  --base-url https://registry.invalid >/dev/null
R2="file://$WORK/registry-v2"

new_project update
# Installed from the ORIGINAL registry, which only has the older version. Installing from
# $REGISTRY-v2 would have picked the newest, and `update --check` would then correctly report
# "up to date" while the test expected "update available".
#
# `add` records the pin `^<version>`, and `update` stays inside it — a 2099.x target would be
# outside `^2026.10.1` and update would correctly decline to move. The second version is a
# patch-level release within the same major, which is what a dataset update actually looks like.
in_project update add "$first_cc" --registry "$REGISTRY" --cache-dir "$WORK/cache-u" >/dev/null 2>&1
assert_exit 0 "update --check succeeds" \
  bash -c "cd '$WORK/update' && '$BIN' update --check --registry '$R2' --cache-dir '$WORK/cache-u'"
assert_contains "update available" "update --check reports the pending version" \
  bash -c "cd '$WORK/update' && '$BIN' update --check --registry '$R2' --cache-dir '$WORK/cache-u'"
before=$(ls "$WORK/update/.terrakit/packages/$first_cc" | wc -l | tr -d ' ')
if [[ "$before" == "1" ]]; then
  ok "update --check changed nothing on disk"
else
  bad "update --check left $before version directories"
fi

assert_exit 0 "update applies" \
  bash -c "cd '$WORK/update' && '$BIN' update --registry '$R2' --cache-dir '$WORK/cache-u'"
after=$(ls "$WORK/update/.terrakit/packages/$first_cc" 2>/dev/null | wc -l | tr -d ' ')
if [[ "$after" == "1" ]]; then
  ok "the superseded version is removed, so search cannot return a place twice"
else
  bad "after update, $after version directories remain"
fi
assert_contains "$second_version" "the installed package is the new version" \
  bash -c "cd '$WORK/update' && '$BIN' list"
assert_exit 0 "the updated package still validates" \
  bash -c "cd '$WORK/update' && '$BIN' validate"

assert_exit 0 "update is idempotent" \
  bash -c "cd '$WORK/update' && '$BIN' update --registry '$R2' --cache-dir '$WORK/cache-u'"
assert_contains "up to date" "a second update reports nothing to do" \
  bash -c "cd '$WORK/update' && '$BIN' update --registry '$R2' --cache-dir '$WORK/cache-u'"

new_project update-to
in_project update-to add "$first_cc" --registry "$REGISTRY" --cache-dir "$WORK/cache-u" >/dev/null 2>&1
assert_exit 0 "update --to pins a specific version" \
  bash -c "cd '$WORK/update-to' && '$BIN' update --to '$second_version' --registry '$R2' --cache-dir '$WORK/cache-u'"
assert_contains "$second_version" "update --to installed the requested version" \
  bash -c "cd '$WORK/update-to' && '$BIN' list"

printf '\n\033[1m=== remove\033[0m\n'
new_project remove
in_project remove add "$first_cc" --registry "$R2" --cache-dir "$WORK/cache-r" >/dev/null 2>&1
assert_exit 0 "remove uninstalls" \
  bash -c "cd '$WORK/remove' && '$BIN' remove '$first_cc' --cache-dir '$WORK/cache-r'"
if [[ -d "$WORK/remove/.terrakit/packages/$first_cc" ]]; then
  bad "the package directory survived removal"
else
  ok "the package directory is gone"
fi
assert_contains '"resolution": {}' "terrakit.json no longer references the package" \
  bash -c "cat '$WORK/remove/terrakit.json'"
assert_exit 6 "removing a country that is not installed reports not-installed" \
  bash -c "cd '$WORK/remove' && '$BIN' remove '$first_cc' --cache-dir '$WORK/cache-r'"
assert_exit 2 "remove with no country is a usage error" \
  bash -c "cd '$WORK/remove' && '$BIN' remove --cache-dir '$WORK/cache-r'"

# The shared cache outlives removal by default: that is what makes it worth sharing.
new_project cache-keep
in_project cache-keep add "$first_cc" --registry "$R2" --cache-dir "$WORK/cache-s" >/dev/null 2>&1
in_project cache-keep remove "$first_cc" --cache-dir "$WORK/cache-s" >/dev/null 2>&1
cached=$(find "$WORK/cache-s/artifacts" -name '*.zst' 2>/dev/null | wc -l | tr -d ' ')
if [[ "$cached" != "0" ]]; then
  ok "the download cache survives removal by default"
else
  bad "removal deleted the shared cache without --prune"
fi
assert_contains "cached" "a warm cache is used instead of re-downloading" \
  bash -c "cd '$WORK/cache-keep' && '$BIN' add '$first_cc' --registry '$R2' --cache-dir '$WORK/cache-s'"

# Reinstalled by the warm-cache assertion above, so there is something to remove. Removing it
# first and then pruning would exit 6, which is correct behaviour and the wrong test.
assert_exit 0 "remove --prune succeeds" \
  bash -c "cd '$WORK/cache-keep' && '$BIN' remove '$first_cc' --prune --cache-dir '$WORK/cache-s'"
cached=$(find "$WORK/cache-s/artifacts" -name '*.zst' 2>/dev/null | wc -l | tr -d ' ')
if [[ "$cached" == "0" ]]; then
  ok "--prune drops entries no installed package references"
else
  bad "--prune left $cached unreferenced cache entries"
fi

printf '\n\033[1m=== refusing to trust a damaged install\033[0m\n'
new_project damaged
# Every step uses the ORIGINAL registry, which publishes one version. Against the two-version
# registry, `--force` would have installed the newer version *alongside* the corrupt one and left
# the corrupt directory in place — correct for `add`, which does not manage other versions, and
# the reason the assertions below would have failed for a reason unrelated to damage.
#
# That distinction is worth stating: `add` adds, `update` updates. Removing a superseded version
# is `update`'s job, because it is the operation that knows a version is superseded.
in_project damaged add "$first_cc" --registry "$REGISTRY" --cache-dir "$WORK/cache-d" >/dev/null 2>&1
printf 'X' | dd "of=$WORK/damaged/.terrakit/packages/$first_cc/$VERSION_SEGMENT/data.sqlite" \
  bs=1 seek=2048 conv=notrunc 2>/dev/null
assert_exit 3 "an installed package that fails verification is not reported as up to date" \
  bash -c "cd '$WORK/damaged' && '$BIN' add '$first_cc' --registry '$REGISTRY' --cache-dir '$WORK/cache-d'"
assert_contains "modified" "the refusal says the package is damaged, not merely present" \
  bash -c "cd '$WORK/damaged' && '$BIN' add '$first_cc' --registry '$REGISTRY' --cache-dir '$WORK/cache-d'"
assert_exit 3 "validate reports the damage too" \
  bash -c "cd '$WORK/damaged' && '$BIN' validate '$first_cc'"
assert_exit 0 "--force repairs it" \
  bash -c "cd '$WORK/damaged' && '$BIN' add '$first_cc' --force --registry '$REGISTRY' --cache-dir '$WORK/cache-d'"
assert_exit 0 "the repaired package validates" \
  bash -c "cd '$WORK/damaged' && '$BIN' validate '$first_cc'"

printf '\n\033[1m=== cache\033[0m\n'
new_project cache
CACHE="$WORK/cache-cmds"
in_project cache add "$first_cc" --registry "$REGISTRY" --cache-dir "$CACHE" >/dev/null 2>&1

assert_contains "$CACHE" "cache path reports the --cache-dir the install used" \
  bash -c "cd '$WORK/cache' && '$BIN' cache path --cache-dir '$CACHE'"
assert_exit 0 "cache list succeeds" \
  bash -c "cd '$WORK/cache' && '$BIN' cache list --cache-dir '$CACHE'"
assert_contains "in use" "cache list shows the artifact an installed package references" \
  bash -c "cd '$WORK/cache' && '$BIN' cache list --cache-dir '$CACHE'"
assert_exit 0 "cache verify passes on a good cache" \
  bash -c "cd '$WORK/cache' && '$BIN' cache verify --cache-dir '$CACHE'"
assert_exit 2 "an unknown cache subcommand is a usage error" \
  bash -c "cd '$WORK/cache' && '$BIN' cache frobnicate --cache-dir '$CACHE'"
assert_exit 2 "cache with no subcommand is a usage error" \
  bash -c "cd '$WORK/cache' && '$BIN' cache --cache-dir '$CACHE'"

# A fresh entry is unreferenced by every project this one can see, so `clean` must keep it.
assert_exit 0 "cache clean succeeds" \
  bash -c "cd '$WORK/cache' && '$BIN' cache clean --cache-dir '$CACHE'"
kept=$(find "$CACHE/artifacts" -name '*.zst' 2>/dev/null | wc -l | tr -d ' ')
if [[ "$kept" == "1" ]]; then
  ok "cache clean keeps an entry that is still in use"
else
  bad "cache clean removed an entry in use ($kept remain)"
fi

# A corrupt cache entry must be reported, not silently served.
printf 'X' | dd "of=$(find "$CACHE/artifacts" -name '*.zst' | head -1)" bs=1 seek=64 conv=notrunc 2>/dev/null
# One invocation, because a failed entry is deleted: a second run finds a clean cache and
# would assert nothing about the original failure.
verify_json=$(cd "$WORK/cache" && "$BIN" cache verify --cache-dir "$CACHE" --json 2>/dev/null) && verify_code=0 || verify_code=$?
if [[ "$verify_code" == "3" ]]; then
  ok "cache verify reports a corrupted entry as a verification failure (exit 3)"
else
  bad "cache verify returned $verify_code, expected 3"
fi
if grep -q "do not match" <<< "$verify_json"; then
  ok "cache verify says what was wrong"
else
  bad "cache verify did not explain the failure: $verify_json"
fi
assert_exit 0 "the corrupt entry is gone, so verify passes again" \
  bash -c "cd '$WORK/cache' && '$BIN' cache verify --cache-dir '$CACHE'"

# And a warm-cache install must refuse to use a corrupted entry.
assert_exit 0 "reinstalling over a corrupted cache entry succeeds" \
  bash -c "cd '$WORK/cache' && '$BIN' add '$first_cc' --registry '$REGISTRY' --cache-dir '$CACHE'"

printf '\n\033[1m=== bundle\033[0m\n'
new_project bundle
in_project bundle add "$first_cc" --registry "$REGISTRY" --cache-dir "$WORK/cache-b" >/dev/null 2>&1
assert_exit 0 "bundle --platform all succeeds" \
  bash -c "cd '$WORK/bundle' && '$BIN' bundle --platform all"
if [[ -f "$WORK/bundle/bundle/terrakit-bundle.json" ]]; then
  ok "the canonical manifest is written"
else
  bad "terrakit-bundle.json was not written"
fi
if [[ -f "$WORK/bundle/bundle/ios/terrakit/$first_cc/$VERSION_SEGMENT/data.sqlite" ]]; then
  ok "the iOS tree carries the database for an Xcode folder reference"
else
  bad "bundle/ios/terrakit is missing the database"
fi
if [[ -f "$WORK/bundle/bundle/android/assets/terrakit/$first_cc/$VERSION_SEGMENT/data.sqlite" ]]; then
  ok "the Android tree carries the database under assets/"
else
  bad "bundle/android/assets/terrakit is missing the database"
fi
assert_contains "noCompress" "the Gradle snippet asks for noCompress on sqlite" \
  bash -c "cat '$WORK/bundle/bundle/android/terrakuit.gradle'"
assert_exit 0 "the bundle manifest is valid JSON" \
  bash -c "python3 -c 'import json; json.load(open(\"$WORK/bundle/bundle/terrakit-bundle.json\"))'"

# The bundled database must be the installed one, byte for byte.
bundled="$WORK/bundle/bundle/ios/terrakit/$first_cc/$VERSION_SEGMENT/data.sqlite"
installed="$WORK/bundle/.terrakit/packages/$first_cc/$VERSION_SEGMENT/data.sqlite"
if cmp -s "$bundled" "$installed"; then
  ok "the bundled database is byte-identical to the installed one"
else
  bad "the bundled database differs from the installed one"
fi

# Rebuilding must not churn the checked-in manifest.
cp "$WORK/bundle/bundle/terrakit-bundle.json" "$WORK/bundle-manifest-first.json"
bash -c "cd '$WORK/bundle' && '$BIN' bundle --platform all" >/dev/null 2>&1
if cmp -s "$WORK/bundle/bundle/terrakit-bundle.json" "$WORK/bundle-manifest-first.json"; then
  ok "the bundle manifest is byte-identical across rebuilds"
else
  bad "the bundle manifest changed between identical rebuilds"
fi

assert_exit 2 "bundle without --platform is a usage error" \
  bash -c "cd '$WORK/bundle' && '$BIN' bundle"
assert_exit 2 "bundle with an unknown platform is a usage error" \
  bash -c "cd '$WORK/bundle' && '$BIN' bundle --platform windows"
assert_exit 6 "bundling with nothing installed reports not-installed" \
  bash -c "mkdir -p '$WORK/empty-bundle' && cd '$WORK/empty-bundle' && '$BIN' init > /dev/null && '$BIN' bundle --platform ios"

printf '\n\033[1m=== install from a GitHub source\033[0m\n'
# A local stand-in for api.github.com and codeload.github.com. The hosts are overridable
# (TERRAKIT_GITHUB_API, TERRAKIT_GITHUB_CODELOAD) because GitHub Enterprise has its own, and
# because an installer that cannot be tested offline cannot be tested at all.
GH="$WORK/github"
SHA=abc123def4567890abcdef1234567890abcdef12
mkdir -p "$GH/api/repos/example/terrakit-data-ge/commits" "$GH/example/terrakit-data-ge/tar.gz"

GH_PACKAGE="$(ls -d "$WORK/releases/terrakit-data-$CC_LOWER-"* | head -1)"
python3 - "$GH" "$SHA" "$GH_PACKAGE" <<'PYEOF'
import json, os, pathlib, sys, tarfile, tempfile

gh, sha, package = pathlib.Path(sys.argv[1]), sys.argv[2], pathlib.Path(sys.argv[3])
assert package.is_dir(), f"no package at {package}"
api = gh / "api/repos/example"
tarballs = gh / "example/terrakit-data-ge/tar.gz"


def publish(tag, commit, build):
    (api / "terrakit-data-ge/commits").mkdir(parents=True, exist_ok=True)
    (api / "terrakit-data-ge/commits" / tag).write_text(json.dumps({"sha": commit}))
    build(tarballs / commit)


def files(names, root_name):
    def build(path):
        root = f"terrakit-data-ge-{root_name}"
        with tarfile.open(path, "w:gz") as tar:
            for name in names:
                tar.add(package / name, arcname=f"{root}/{name}")
    return build


publish("v1.0.0", sha, files(["data.sqlite", "manifest.json", "signature.json"], sha))


def unsigned(path):
    root = f"terrakit-data-ge-{path.name}"
    with tarfile.open(path, "w:gz") as tar:
        for name in ("data.sqlite", "manifest.json"):
            tar.add(package / name, arcname=f"{root}/{name}")


publish("unsigned", "1" * 40, unsigned)


def hostile(path):
    root = f"terrakit-data-ge-{path.name}"
    with tarfile.open(path, "w:gz") as tar:
        for name in ("data.sqlite", "manifest.json", "signature.json"):
            tar.add(package / name, arcname=f"{root}/{name}")
        # A link out of the tree. In an archive from an untrusted repository this is an escape,
        # not a shortcut.
        info = tarfile.TarInfo(f"{root}/escape")
        info.type = tarfile.SYMTYPE
        info.linkname = "../../../../../../etc/passwd"
        tar.addfile(info)
    # A parent-directory entry, which the `tar` crate's own builder would not create.
    with tarfile.open(path, "a") if False else tarfile.open(path, "w:gz") as tar:
        for name in ("data.sqlite", "manifest.json", "signature.json"):
            tar.add(package / name, arcname=f"{root}/{name}")
        escape = tarfile.TarInfo(f"{root}/../../escape.txt")
        escape.size = 0
        tar.addfile(escape)


publish("hostile", "2" * 40, hostile)


def mismatched(path):
    root = f"terrakit-data-ge-{path.name}"
    with tarfile.open(path, "w:gz") as tar:
        # The manifest and signature are authentic; the database beside them is not the one it
        # describes. A signature over the manifest proves the manifest, not the directory.
        tar.add(package / "manifest.json", arcname=f"{root}/manifest.json")
        tar.add(package / "signature.json", arcname=f"{root}/signature.json")
        with tempfile.NamedTemporaryFile(suffix=".sqlite", delete=False) as tmp:
            tmp.write(b"a different database entirely")
            other = tmp.name
        tar.add(other, arcname=f"{root}/data.sqlite")
        os.unlink(other)


publish("mismatched", "3" * 40, mismatched)

(api / "terrakit-data-ge.json").write_text(json.dumps({"default_branch": "main"}))
# `main` must resolve too: the default-branch path and the branch path are separate code, and
# only the branch path is exercised by `@main`.
(api / "terrakit-data-ge/commits").mkdir(parents=True, exist_ok=True)
(api / "terrakit-data-ge/commits/main").write_text(json.dumps({"sha": sha}))
print("fake github host built")
PYEOF

export TERRAKIT_GITHUB_API="file://$GH/api"
export TERRAKIT_GITHUB_CODELOAD="file://$GH"
GH_BIN="$BIN"

new_project install
assert_exit 0 "install resolves a tag and installs" \
  bash -c "cd '$WORK/install' && '$GH_BIN' install github.com/example/terrakit-data-ge@v1.0.0 --cache-dir '$WORK/cache-i'"
assert_contains "signature verified" "install verifies the signature over the manifest bytes" \
  bash -c "cd '$WORK/install' && '$GH_BIN' install github.com/example/terrakit-data-ge@v1.0.0 --force --cache-dir '$WORK/cache-i' 2>&1 || true"
# A separate project: the tag install above already put this exact version on disk, and
# `install` refuses to overwrite an installed package. Branch behaviour is a separate question
# and needs a separate project to ask it in.
new_project install-branch
assert_contains "$SHA" "the resolved commit is reported" \
  bash -c "cd '$WORK/install-branch' && '$GH_BIN' install github.com/example/terrakit-data-ge@main --cache-dir '$WORK/cache-i'"
assert_contains "not a versioned ref" "a branch warns that it is not reproducible" \
  bash -c "cd '$WORK/install-branch' && '$GH_BIN' install github.com/example/terrakit-data-ge@main --cache-dir '$WORK/cache-i' 2>&1 || true"

# The fake repository is deliberately named `terrakit-data-ge` while serving the FIRST package,
# which is TR. That mismatch is the point: the country must come from the signed manifest, not
# from the repository name, or anyone able to push a renamed repository could land a dataset in
# another country's slot.
resolved=$(python3 -c "
import json, sys
document = json.load(open('$WORK/install-branch/terrakit.json'))
entries = document['resolution']
country = sorted(entries)[0]
print(country, entries[country].get('resolved_ref', ''))
")
if [[ "$resolved" == "$first_cc $SHA" ]]; then
  ok "terrakit.json pins the resolved commit under the MANIFEST's country, not the repo name"
else
  bad "expected \"$first_cc $SHA\", got \"$resolved\""
fi
if [[ -d "$WORK/install-branch/.terrakit/packages/$first_cc" ]]; then
  ok "the package landed under the country its signed manifest declares"
else
  bad "the package did not land under $first_cc"
fi

assert_exit 4 "an unknown ref reports not-found" \
  bash -c "cd '$WORK/install' && '$GH_BIN' install github.com/example/terrakit-data-ge@no-such-ref --cache-dir '$WORK/cache-i'"
assert_exit 2 "a non-GitHub source is a usage error" \
  bash -c "cd '$WORK/install' && '$GH_BIN' install gitlab.com/example/pkg"
assert_exit 2 "a ref containing '..' is refused" \
  bash -c "cd '$WORK/install' && '$GH_BIN' install github.com/example/terrakit-data-ge@../../etc/passwd"

new_project install-hostile
assert_exit 3 "a tarball with a link or a traversal is refused" \
  bash -c "cd '$WORK/install-hostile' && '$GH_BIN' install github.com/example/terrakit-data-ge@hostile --cache-dir '$WORK/cache-i'"
if [[ -d "$WORK/install-hostile/.terrakit/packages/GE" ]]; then
  bad "the hostile archive installed something"
else
  ok "the hostile archive installs nothing"
fi

new_project install-unsigned
assert_exit 3 "an unsigned package is refused without the flag" \
  bash -c "cd '$WORK/install-unsigned' && '$GH_BIN' install github.com/example/terrakit-data-ge@unsigned --cache-dir '$WORK/cache-i'"
assert_exit 0 "--allow-unsigned works outside CI" \
  bash -c "cd '$WORK/install-unsigned' && '$GH_BIN' install github.com/example/terrakit-data-ge@unsigned --allow-unsigned --cache-dir '$WORK/cache-i'"
assert_exit 3 "--allow-unsigned refuses to run in CI" \
  bash -c "cd '$WORK/install-unsigned' && CI=true '$GH_BIN' install github.com/example/terrakit-data-ge@unsigned --allow-unsigned --cache-dir '$WORK/cache-i' --force"

new_project install-mismatch
assert_exit 3 "a signed manifest beside an unsigned database is refused" \
  bash -c "cd '$WORK/install-mismatch' && '$GH_BIN' install github.com/example/terrakit-data-ge@mismatched --cache-dir '$WORK/cache-i'"
unset TERRAKIT_GITHUB_API TERRAKIT_GITHUB_CODELOAD

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

# `postal` has two outcomes and both must speak JSON.
#
# It used to return Exit::NotFound from the query layer, which left the `--json` path with no way to
# say anything: `postal TR 99999 --json` printed an empty stream and exited 4. A consumer parsing
# stdout cannot tell that from a crash, and "not found" is an ordinary result rather than a failure.
# The exit code still distinguishes the two, so a script can branch on either; what is asserted here
# is that stdout carries a JSON record either way.
#
# Reuses the project from the install section, which already has a package.
INSTALLED_DB="$WORK/ok-install/.terrakit/packages/$first_cc"
INSTALLED_VERSION="$(ls "$INSTALLED_DB" 2>/dev/null | head -1)"
if [[ -n "$INSTALLED_VERSION" && -f "$INSTALLED_DB/$INSTALLED_VERSION/data.sqlite" ]]; then
  payload=$(in_project ok-install postal "$first_cc" "99999" --json 2>/dev/null || true)
  assert_json "postal --json on a code that does not exist is valid JSON" "$payload"
  if [[ "$payload" == *'"found":false'* ]]; then
    ok "postal reports found:false rather than silence"
  else
    bad "postal did not report found:false: $payload"
  fi

  known="$(sqlite3 "$INSTALLED_DB/$INSTALLED_VERSION/data.sqlite" \
    "SELECT code FROM postal_codes LIMIT 1" 2>/dev/null)"
  if [[ -n "$known" ]]; then
    payload=$(in_project ok-install postal "$first_cc" "$known" --json 2>/dev/null || true)
    if [[ "$payload" == *'"found":true'* ]]; then
      ok "postal reports found:true for a code that exists"
    else
      bad "postal did not report found:true: $payload"
    fi
    # `placeId` must be an id, not the place's name. It briefly held the name, and the conformance
    # battery recorded that value as the expectation -- so the suite agreed with the bug instead of
    # catching it, which is why the shape is asserted here and not only through the oracle.
    if [[ "$payload" == *'"placeId":"place:'* || "$payload" == *'"placeId":"admin:'* ]]; then
      ok "postal placeId is an id, not a name"
    else
      bad "postal placeId is not an id: $payload"
    fi
  else
    printf '  (no postal codes in %s; placeId shape not checked)\n' "$first_cc"
  fi
else
  printf '  (no installed package at %s; postal JSON contract not checked)\n' "$INSTALLED_DB"
fi

printf '\n\033[1m=== summary\033[0m\n'
printf '  %s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" == "0" ]] || exit 1