# TerraKit CLI Specification

Status: **Draft — surface defined, not implemented**
Last updated: 2026-10-05
Implementation language: Rust ([`ADR-007`](docs/adr/ADR-007-language-stack.md))

The command surface below is normative. Argument names, exit codes and the
stdout/stderr split are contract; `terrakit` is not written yet, so this document
is what it will be built against.

---

## 1. Global rules

These hold for every command and are the reason the CLI is scriptable.

| Rule | Detail |
|---|---|
| **stdout is data, stderr is progress** | A human watching a download sees a progress bar. A script parsing `--json` gets JSON and nothing else. Mixing them is the single most common way a CLI breaks CI. |
| **Exit code 0 means success** | Anything non-zero means the stated thing did not happen. |
| **Never prompt unless asked** | No interactive TTY check. If an operation would prompt, it takes an explicit flag. |
| **No colour when piped** | Auto-detect. `NO_COLOR` is honoured. |
| **Identifiers are opaque** | The CLI never prints a parsed ID. `--explain` may print its parts, as a debugging aid. |
| **`--json` everywhere it makes sense** | Machine-readable output on every read command. |

### Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | runtime failure (network, filesystem, corrupt data) |
| `2` | usage error (unknown flag, missing argument, bad ISO code) |
| `3` | verification failure — **checksum, signature, or schema mismatch** |
| `4` | nothing found (a search with no matches is not an error) |
| `5` | disk space insufficient |
| `6` | package not installed, when the command required one |

Exit code `3` is separate from `1` on purpose. CI needs to distinguish "the
download broke" from "the package is not trustworthy", and a script that cannot
tell them apart cannot enforce a security policy.

---

## 2. Commands

The set required by SPEC §49, plus `bundle`, which the specification's mobile
story needs and does not mention
([`ARCHITECTURE.md` §6](ARCHITECTURE.md#6-mobile-packaging)).

```text
terrakit init                    create terrakit.json in the current directory
terrakit add <CC>                install or upgrade a country package
terrakit remove <CC>             uninstall a country package
terrakit update [CC]             check for and apply newer dataset versions
terrakit list                    installed packages
terrakit search <query>          search across installed packages
terrakit info <CC>               metadata and entity counts for a country
terrakit validate [CC]           run the SPEC section 30 checks
terrakit doctor                  environment and installation diagnostics
terrakit cache <subcommand>      manage the shared download cache
terrakit version                 version of the CLI, and of the keyring
terrakit install <source>        install from a GitHub source
terrakit bundle --platform <p>   emit platform resource manifests
```

### 2.1 `init`

```bash
terrakit init
terrakit init --name my-app --level postal
```

Writes `terrakit.json` ([`PACKAGE_FORMAT.md` §9](PACKAGE_FORMAT.md#9-terrakitjson)) and
`.terrakit/`. Refuses to overwrite an existing `territakit.json` without `--force`, and
`--force` still backs the old file up rather than deleting it.

```json
{
  "schemaVersion": 1,
  "packages": {},
  "level": "cities"
}
```

An empty `packages` map is valid and means "nothing installed yet".

### 2.2 `add`

```bash
terrakit add TR
terrakit add TR --basic
terrakit add TR --cities
terrakit add TR --postal
terrakit add DE GE US          # several countries in one invocation
terrakit add TR --version 2026.10.1
terrakit add TR --level postal --json
```

Levels, per [`PACKAGE_FORMAT.md` §4](PACKAGE_FORMAT.md#4-levels): `basic`,
`administrative`, `cities` (default), `postal`, `full`. `streets` is accepted and
rejected with a clear message until OSM lands
([`ADR-004`](docs/adr/ADR-004-geonames-first-osm-later.md)) — a flag the user
expects from the specification must fail with an explanation, not "unknown flag".

Several countries at once is not in the specification. It is included because
`terrakit add TR DE GE` is what people actually type, and one invocation with one
lock acquisition is simpler than three.

Output goes to stderr, in the order of
[`PACKAGE_FORMAT.md` §7](PACKAGE_FORMAT.md#7-install-flow):

```text
  Fetching registry index                                        0.4s
  Downloading terrakit-data-tr-2026.10.1                          8.9s
    ████████████████████████████░░░░░░░░  62%  5.5 MB/8.9 MB
  Checking manifest signature (ed25519, keyId terrakit-release-2026)   ✓
  Checking SHA-256 (9f2a1c...e4b7)                                  ✓
  Decompressing                                                   1.2s
  Verifying extracted file                                        ✓
  Checking schema compatibility (schemaVersion 1)                 ✓
  Installing to .terrakit/packages/TR/2026.10.1                   ✓

  ✓ Installed terrakit-data-tr@2026.10.1 (cities, 5091 places, 8.9 MB)
  Data generated from GeoNames (CC BY 4.0). Attribution is required in your app.
```

The attribution line is printed on every successful install. SPEC §29 says the
project must never hide source attribution; a user who never opens the docs
should still learn that they owe an attribution.

### 2.3 `remove`

```bash
terrakit remove TR
terrakit remove TR --keep-cache
```

Removes `.terrakit/packages/TR/` and updates `terrakit.json`. `--keep-cache` leaves
the download, which is the point of a shared cache.

`--prune` is the opposite and is the only way cache entries are deleted: an entry is dropped
only when no installed manifest names its digest, so one project cannot delete an artifact
another is about to install. `--keep-cache` wins if both are given.

Integrity is reported before deletion, because afterwards there is nothing left to report — and
a package that was already corrupt is usually the reason the user wanted it gone. Removing a
country that is not installed exits `6`; removing several where only some are installed succeeds
and exits `0`, because the useful part of the command worked.

Only the *selected* version is reported as removed, and `remove` does not manage other versions of
the same country. Upgrading is `update`'s job, because that is the operation that knows a version
has been superseded.

### 2.4 `update`

```bash
terrakit update
terrakit update TR
terrakit update --check          # report only, change nothing
terrakit update --to 2026.10.0  # pin to a specific version
```

`--check` is the default in CI, because an `update` that silently changes data
under a test run is a bad day. Never prompts. Never upgrades across a major
version of `packageFormatVersion` without `--force`, since that needs a newer CLI
than the user may have.

`--to` and `--version` are the same flag here; `--to` on `add` is a usage error naming the
alternative, rather than a silently accepted no-op.

Two rules the implementation makes explicit, because both are silent when broken:

- **The project's pin is respected.** `add` records `^<version>`, and `update` stays inside it. An
  explicit `--to` overrides the pin for that run.
- **The superseded version is removed.** Two versions of one country installed means `search`
  returns the same places twice with identical IDs, and a caller cannot tell a duplicate from a
  real second result. A version already on disk and still verifying is adopted rather than
  re-downloaded.

Exit codes: `1` if any package could not be updated, `6` if nothing is installed, `4` for an
unsatisfiable pin. `--check` exits `0` when every package is resolvable, whether or not updates are
available — availability is reported in the output, not in the status.

### 2.5 `list`

```bash
terrakit list
terrakit list --json
```

```text
  CC  Package                            Version     Level   Size   Integrity
  TR  terrakit-data-tr                   2026.10.1   cities  8.9 MB verified
  DE  terrakit-data-de                   2026.09.4   cities  12 MB  verified
```

Shows the integrity state from the lockfile, so tampering in `.terrakit/` is
visible without running a separate command.

### 2.6 `search`

```bash
terrakit search Antalya
terrakit search Antalya --country TR
terrakit search Antalya --city Antalya --limit 20
terrakit search Antalya --type city,town --json
```

```text
  CITY       Antalya                Antalya, Türkiye            1 335 002
  DISTRICT   Muratpaşa              Antalya, Türkiye              300 000
  DISTRICT   Kepez                  Antalya, Türkiye              262 000

  3 results in 14 ms
```

Ranking is the deterministic order in
[`DATA_MODEL.md` §9](DATA_MODEL.md#9-search-schema): exact, prefix, fuzzy by phonetic
distance, then precomputed population rank, tie-broken by ID. The CLI and both
SDKs must return identical ordering for the same bytes, which is what
`tests/integration/conformance/` checks.

Exit code `4` when there are no matches, so a script can distinguish "empty result"
from "the search broke".

### 2.7 `info`

```bash
terrakit info TR
terrakit info TR --json
```

```text
  Türkiye (TR / TUR / 792)
    Continent    Asia
    Capital      Ankara
    Currency     TRY  Turkish Lira
    Phone        +90
    TLD          .tr
    Driving      right
    Timezone     Europe/Istanbul
    Languages    tr, ku, diq, az, av

    Dataset      terrakit-data-tr 2026.10.1
    Level        cities
    Generated    2026-10-01 (GeoNames 2026-09-01)
    Places       5091
    Postal       2953 codes, 4946 areas
    License      CC-BY-4.0
    Attribution  Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0.
```

`--json` output is the machine-readable equivalent of this and is the form the
SDKs' `Terra.attribution` mirrors.

### 2.8 `validate`

```bash
terrakit validate
terrakit validate TR
terrakit validate TR --json
terrakit validate --strict      # treat warnings as errors
```

Runs the SPEC §30 checks against the installed package. Output shape from §30:

```text
✓ Country structure: 1
✓ Administrative areas (level 2): 81
✓ Districts (level 3): 1006
✓ Cities: 112
✓ Towns: 710
✓ Villages: 3089
✓ Neighborhoods: 124
✓ Postal codes: 2953
✓ Postal areas: 4946
✓ Places with coordinates: 5072

  ! [duplicate-names] 2 places named 'karakoy' under the same parent [place:geonames:8630861]

Errors: 0
Warnings: 9
```

Settlement counts are by **type**, not level. Türkiye's largest cities sit at
level 3, so counting level 5 prints `Cities: 0` for a package that has 112 of
them — a report that contradicts its own package is worse than no report.

`--strict` is for CI and release. Warnings are real information about upstream
data quality and are never hidden; `--strict` only decides whether they fail the
build.

### 2.9 `doctor`

```bash
terrakit doctor
terrakit doctor --json
```

Diagnostics only. Mutates nothing — a command a user runs when something is wrong
must not make it worse.

```text
  CLI           0.1.0 (release)
  Keyring       terrakit-release-2026, terrakit-release-2025 (2 trusted keys)
  SQLite        3.54.0 (fts5 ✓, rtree ✓, trigram ✓)
  Platform      macOS 27.0, arm64
  Network       reachable (registry 142 ms)
  Project       /Users/you/app, terrakit.json v1
  Packages      2 installed, both verified
  Cache         21 MB in 2 entries, prunable: 8.4 MB
  Free space    41 GB available
```

`doctor` is what a bug report should quote, which is why it is specified rather
than improvised.

### 2.10 `cache`

```bash
terrakit cache list
terrakit cache clean              # remove prunable entries
terrakit cache clean --all        # remove everything
terrakit cache path               # print the cache directory
terrakit cache verify             # re-check cached checksums
```

The cache is shared across projects, so `clean` without `--all` only removes
entries no project references. `clean --all` is always safe: it is a cache.

Two things the implementation makes explicit, because both are silent when wrong:

- **"Unreferenced" means unreferenced by anything this command can see.** Only this project's
  installed packages are visible, so an entry another project installed a minute ago is
  unreferenced by definition. `clean` therefore also applies a recency floor
  ([`PRUNE_MIN_AGE`](cli/src/cache.rs), 7 days): an entry younger than that is left alone whatever
  its state, because the overwhelmingly common reason an entry is young is that something is
  installing or has just installed it. `clean --all` is the honest full delete.
- **A cache entry is a claim, not a fact.** Its filename is a digest, but an interrupted write, a
  truncated file or a poisoned entry would otherwise be served silently, because the fast path
  skips the download and with it the checksum. `add` re-verifies before use; `cache verify`
  re-checks everything at once and **deletes** every entry that fails, since such an entry has no
  value and would make the next `add` re-download only to fail again.

`cache verify` exits `3` on any failure, matching every other verification failure in the CLI, so
CI needs no special case for a corrupt cache. `cache path` prints the bare path with no decoration
— it is what a build script interpolates.

### 2.11 `install`

```bash
terrakit install github.com/example/terrakit-data-tr
terrakit install github.com/example/terrakit-data-tr@1.2.0
terrakit install github.com/example/terrakit-data-tr@main
terrakit install github.com/example/terrakit-data-tr@a1b2c3d
```

Per [`PACKAGE_FORMAT.md` §10](PACKAGE_FORMAT.md#10-github-package-sources). A GitHub URL is
an untrusted transport, not a trust signal: signature verification applies exactly as for
a registry package, and `--allow-unsigned` exists for local development, warns on every
invocation, and refuses to run in CI.

Branch and commit references resolve to a concrete SHA, printed and pinned in the
lockfile.

Implementation notes:

- **The ref resolves before the download, and the download uses the SHA.** A branch that moves
  between the API call and the fetch cannot change the bytes, and `terrakit.json` records
  `resolvedRef` so a later run gets the same package even though `main` may have moved.
- **A non-reproducible ref warns; it does not refuse.** Refusing `@main` would make the documented
  workflow impossible, and the pinned SHA is what actually provides reproducibility.
- **A hostile archive exits `3`.** `CLI_SPEC.md` §1 words that code as "checksum, signature, or
  schema mismatch", but the principle is "this package must not be trusted", and a tarball that
  tries to write outside the extraction directory is exactly that. At `1` it would look like a
  transient error: a CI script keying on `3` would not stop the build, and a retry would fetch the
  same archive.
- **A signed manifest beside a different database is refused.** The signature proves the manifest;
  it says nothing about the directory it sits in, so every file is checked against the manifest's
  own digests after extraction.
- **`TERRAKIT_GITHUB_API` and `TERRAKIT_GITHUB_CODELOAD`** override the hosts for GitHub
  Enterprise, air-gapped mirrors, and testing without a network.

### 2.12 `bundle`

```bash
terrakit bundle --platform ios
terrakit bundle --platform android
terrakit bundle --platform all
```

Not in the specification. Required to make SPEC §57 work on mobile, where an app
reads only from its bundle and `terrakit add` writes to `.terrakit/`
([`ARCHITECTURE.md` §6](ARCHITECTURE.md#6-mobile-packeting)).

Emits a checked-in resource manifest — an Xcode resource list and, for Gradle, an
asset registration. Declared rather than generated at build time so CI stays
hermetic and does not need network access.

```text
bundle/
├── terrakit-bundle.json          canonical manifest, platform-independent
├── ios/terrakit/<CC>/<version>/  add as an Xcode folder reference
└── android/
    ├── assets/terrakit/<CC>/<version>/   Gradle ships assets/ verbatim
    └── terrakuit.gradle          noCompress snippet, included by the app
```

Rules the implementation makes explicit:

- **One version per country.** A bundle carries the version `terrakit.json` selected. Shipping two
  would duplicate ~10 MB into a release and give the SDK's first-match resolution two answers.
- **Only `data.sqlite`, `manifest.json` and `signature.json` are copied.** A staging leftover or a
  `.bak` from a crashed run is not a resource, and shipping it is how a dead file ends up in a
  release.
- **Every copied file is verified against the canonical manifest.** The copy is the last
  opportunity — nothing downstream reads these files until the app ships, and an app cannot detect
  a corrupted bundle.
- **`terrakit-bundle.json` is byte-identical across rebuilds.** A checked-in manifest that changes
  on every invocation cannot be reviewed in a diff, which defeats the point of checking it in.
- **`--out <dir>` overrides the destination**, default `bundle/` in the project root.

Android gets an explicit `noCompress` because a SQLite file inside an APK's asset stream is not
seekable — SQLite needs `mmap` and random access — so it would be inflated twice and still
unreadable in place. It is written as a `.gradle` file to include rather than printed, because a
snippet that has to be pasted is a snippet that gets pasted wrong.

---

## 3. Global flags

| Flag | Effect |
|---|---|
| `--json` | machine-readable output on stdout |
| `--quiet` | suppress progress; errors and the result still print |
| `--verbose` | add timings and resolved paths |
| `--no-color` | disable colour; also honours `NO_COLOR` |
| `--registry <url>` | alternate registry, for mirrors and testing |
| `--cache-dir <path>` | override the shared cache location |
| `--timeout <seconds>` | network timeout, default 30 |
| `--allow-unsigned` | GitHub sources only; refuses in CI |
| `--offline` | fail rather than reach the network |

`--offline` is the honest escape hatch for a locked-down CI runner, and it is
distinct from "the queries are offline", which is always true
([`ARCHITECTURE.md` §5](ARCHITECTURE.md#5-offline-first-contract)).

---

## 4. Configuration

Precedence, highest first:

1. command-line flags
2. `.terrakit/config.toml` in the project
3. `~/.config/terrakit/config.toml`
4. built-in defaults

```toml
# ~/.config/terrakit/config.toml
registry = "https://registry.terrakit.dev"
level = "cities"
timeout = 30
cache_dir = "~/.cache/terrakit"
[keys]
trusted = ["terrakit-release-2026", "terrakit-release-2025"]
```

**Trusted keys live in the config file, not only in the binary.** A user who pins
their own signing key must be able to; a user who does not get the compiled-in
keyring. There is no way to add a key without saying so explicitly, and there is
no "trust whatever arrives" option, because a keyring that extends itself from
the network is not a trust root
([`SECURITY.md` §2](SECURITY.md#2-trust-model)).

---

## 5. What the CLI must never do

From the specification and from decisions recorded here.

- **Never write data into a package.** The CLI fetches, verifies and installs. It does
  not generate, patch, or repair datasets.
- **Never execute package content.** Packages are data. There is no post-install script and
  no plugin mechanism.
- **Never reach the network during a query.** `search`, `info`, `validate` and `list` are
  fully offline. `doctor` is the only command that touches the network outside install and
  update, and `--offline` covers it.
- **Never install without a verified signature**, except under `--allow-unsigned`, which
  refuses in CI.
- **Never print an ID in a form that invites parsing.** IDs are opaque
  ([`DATA_MODEL.md` §3](DATA_MODEL.md#3-identity)).
- **Never hide attribution.** The install summary and `info` both print it.
