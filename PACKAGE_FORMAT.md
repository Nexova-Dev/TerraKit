# TerraKit Package Format

Status: **Draft / Accepted for MVP**
Last updated: 2026-10-05
`packageFormatVersion: 1`
Normative. See [`ARCHITECTURE.md`](ARCHITECTURE.md) for design rationale and
[`DATA_MODEL.md`](DATA_MODEL.md) for the database schema carried inside a package.

---

## 1. What a package is

A package is **data and metadata only**. §39 forbids executing package code during installation, so
a package contains no scripts, no native binaries, no dynamic SQL and no loadable extensions. It is
an SQLite database plus a signed JSON manifest.

```text
terrakit-data-tr-2026.10.1/
├── manifest.json           # signed metadata, the only file the installer must trust
├── data.sqlite             # the dataset, schemaVersion per DATA_MODEL.md
├── data.sqlite.zst         # compressed download artifact, removed after install
└── signature.json          # detached Ed25519 signature over manifest.json
```

Four files. That is the entire format. A package can be verified, diffed and versioned without
understanding the schema, and a corrupted package is detected before a single byte is written to the
install location.

---

## 2. Directory layout on disk

```text
.terrakit/
├── terrakit.json                    # project manifest, §10 of the spec
├── packages/
│   ├── TR/
│   │   ├── 2026.10.1/
│   │   │   ├── manifest.json
│   │   │   ├── data.sqlite
│   │   │   └── signature.json
│   │   └── 2026.09.1/               # previous version retained for rollback
│   └── DE/
│       └── 2026.09.4/
├── cache/                           # download cache, shared across projects
│   └── terrakit-data-tr-2026.10.1.data.sqlite.zst
└── .lock                            # advisory lock, prevents concurrent installs
```

Version directories rather than in-place overwrites, because §32 requires updates to be
**backward compatible** and §35 requires a verifiable flow. Keeping the previous version makes
rollback a directory swap, not a re-download. Two versions are retained; older ones are pruned.

The **cache is shared across projects** and keyed by package name and version. A team working on
four apps installs Türkiye once. The `cache` command from §10 manages it.

---

## 3. `manifest.json`

The only file read before verification completes.

```json
{
  "packageFormatVersion": 1,
  "name": "terrakit-data-tr",
  "version": "2026.10.1",
  "country": "TR",
  "level": "cities",
  "schemaVersion": 1,
  "minReaderVersion": 1,

  "generator": {
    "name": "terrakit-pipeline",
    "version": "0.4.0"
  },

  "generatedAt": "2026-10-01T00:00:00Z",
  "sourceDateEpoch": 1759276800,

  "size": {
    "raw": 48234496,
    "compressed": 8934212
  },

  "entities": {
    "countries": 1,
    "places": 6127,
    "cities": 486,
    "districts": 973,
    "neighborhoods": 0,
    "streets": 0,
    "postalCodes": 9079
  },

  "files": [
    {
      "path": "data.sqlite",
      "rawSize": 48234496,
      "compressedSize": 8934212,
      "sha256": "9f2a1c...e4b7",
      "compression": "zstd"
    }
  ],

  "packageSha256": "c41d8e...90af",

  "license": "CC-BY-4.0",
  "sources": [
    {
      "id": "geonames",
      "version": "2026-09-01",
      "license": "CC-BY-4.0",
      "licenseUrl": "https://creativecommons.org/licenses/by/4.0/",
      "attribution": "GeoNames, CC BY 4.0",
      "url": "https://download.geonames.org/export/dump/",
      "retrievedAt": "2026-09-28T12:00:00Z",
      "entityShare": {
        "countries": 1,
        "postalCodes": 9079,
        "places": 0
      }
    }
  ],

  "attribution": [
    "Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0."
  ],

  "dependencies": [],

  "minSdk": {
    "swift": "1.0.0",
    "kotlin": "1.0.0"
  },

  "features": ["search", "autocomplete", "postal", "nearest", "localization"]
}
```

### Field notes

**`packageFormatVersion`** — the on-disk layout contract. Increments on any change that makes older
installers unable to read the package. `1` is this document.

**`schemaVersion`** — the SQLite schema, per [`DATA_MODEL.md` §14](DATA_MODEL.md#14-versioning).
Independent of `packageFormatVersion`: adding a table is a schema change but not a format change.

**`minReaderVersion`** — the lowest `schemaVersion` that can read this package. Enforced by the SDK
at open time with a clear error. This is the mechanism behind §32's backward-compatibility claim,
and it is the reason `schemaVersion` is an integer rather than a string.

**`sourceDateEpoch`** — makes the build reproducible
([`ARCHITECTURE.md` §11](ARCHITECTURE.md#11-reproducibility)). `generatedAt` derives from it, never
from the wall clock, so identical inputs produce an identical manifest and an identical
`packageSha256`. CI asserts this on release builds.

**`sources[].entityShare`** — which entities came from which source. Attribution is not
all-or-nothing: a package can be mostly GeoNames with a slice from another source, and a consumer
under CC-BY needs to know which parts. `entityShare` makes that per-entity rather than
per-package, which is a stricter and more honest reading of §29.

**`dependencies`** — packages that must be installed for full functionality. Empty for MVP:
countries are self-contained. Present for future cross-border packages.

**`features`** — capability flags. An SDK checks these to decide whether to offer autocomplete or
raise `.featureUnavailable`, rather than failing on a missing table. A `--cities` package has no
`postal` feature and the SDK says so explicitly.

**`minSdk`** — advisory lower bounds. A package must remain readable by older SDKs for the core
dataset; this field only covers optional features. It must never gate core reading.

---

## 4. Levels

§9 specifies `--basic`, `--cities`, `--postal`, `--streets`, `--full`. Level is declared per
package and drives which tables are populated.

| Level | Entities included | Typical TR size |
|---|---|---|
| `basic` | country metadata, continents, timezones, languages, currencies | < 100 KB |
| `administrative` | + all admin areas, full hierarchy | ~ 1 MB |
| `cities` | + settlements with coordinates | ~ 8 MB |
| `postal` | + postal codes and their areas | ~ 14 MB |
| `streets` | + streets | out of MVP |
| `full` | everything | out of MVP |

Levels are **cumulative**, so `--postal` is a superset of `--cities`. The pipeline generates the
highest requested level; the SDK never needs the others.

**Decision recorded:** streets are excluded from MVP scope. Street data requires OSM extraction,
which brings ODbL share-alike obligations and changes the license of any package containing it
([`ARCHITECTURE.md` ADR-004](ARCHITECTURE.md#adr-004--geonames-cc-by-first-osm-odbl-later)). The
schema already reserves level 7 and the `street` namespace, so adding streets is a data change and
not a format change.

`terrakit add TR` with no flag installs `cities`.

---

## 5. Integrity

§35 requires SHA-256 checksums and optionally signatures. Both are implemented; signature
verification is the default, not an opt-in flag.

### 5.1 Verification order

```text
1. Download            → registry manifest.json
2. Manifest signature  → Ed25519 over the canonical manifest bytes
3. File checksum       → SHA-256 of each entry in files[]
4. Package checksum    → packageSha256
5. Schema check        → schemaVersion within SDK's supported range
6. Extract             → decompress into a temp directory
7. Post-extract check  → re-verify SHA-256 on the decompressed file
8. Move into place     → atomic rename
```

Step 7 is not redundant. A decompression bug, a truncated stream or a hostile `.zst` can produce
bytes that never matched the download. Verifying after extraction is what actually guarantees the
installed file is the signed file.

Step 8 is a rename within the same filesystem, so an interrupted install never leaves a
half-populated package directory. The previous version stays intact until the new one is verified.

**Nothing is trusted before step 2 passes.** Not the size, not the compressed filename, not
anything read out of the archive. A package whose signature fails is deleted, not kept for
inspection.

### 5.2 Signature scheme

```json
// signature.json
{
  "algorithm": "ed25519",
  "keyId": "terrakit-release-2026",
  "publicKey": "MCowBQYDK2VwAyEA...",
  "signature": "3pQ8mN2v...kLw",
  "signedContent": "manifest.json",
  "algorithmVersion": 1
}
```

Ed25519, chosen over RSA/ECDSA for a fixed-size signature, no parameter negotiation and no
certificate chain to validate. Canonical manifest bytes are what is signed, so the JSON parser runs
**after** verification and cannot be used to smuggle a second interpretation of the same document.
This is the JSON-signature-confusion class of bug; signing the parsed object instead of the bytes is
how it gets introduced.

`keyId` supports rotation. The SDK trusts a keyring shipped in the SDK; a rotated key is added in a
new SDK release while the old key stays trusted until packages signed with it expire. Key
compromise is handled by shipping a revocation list in a new SDK release — see
[`SECURITY.md`](SECURITY.md) for the full scheme and the recovery path.

A package signed by an untrusted key **fails closed**. It does not prompt, and it does not install
with a warning. Fail-closed is the only behaviour consistent with §35.

---

## 6. Compression

Zstandard, per §34.

```text
level 19          # packages are downloaded once and read many times
long=30           # improves ratio on large SQLite files
no dictionary     # keeps packages independently decompressible
```

Dictionary compression is rejected deliberately: it would make each package decompressible only in
the presence of its siblings, which conflicts with §11's ability to install a single package from an
arbitrary GitHub repository.

`packageFormatVersion` covers the compression parameters. A future change to the compression scheme
increments it, and the installer selects the algorithm from the manifest rather than guessing from
the extension.

Size impact measured on the TR `postal` fixture: 13.9 MB raw → 4.1 MB compressed, roughly 3.4×.
The manifest carries both numbers so consumers can reason about the trade before downloading.

---

## 7. Install flow

```text
terrakit add TR
```

```text
  Resolving level: cities
  Fetching registry index                                  0.4s
  Downloading terrakit-data-tr-2026.10.1                    8.9s
    ████████████████████████████░░░░░░░░  62%  5.5 MB/8.9 MB
  Checking manifest signature (ed25519, keyId terrakit-release-2026)   ✓
  Checking SHA-256 (9f2a1c...e4b7)                          ✓
  Decompressing                                             1.2s
  Verifying extracted file                                  ✓
  Checking schema compatibility (schemaVersion 1)           ✓
  Installing to .terrakit/packages/TR/2026.10.1             ✓

  ✓ Installed terrakit-data-tr@2026.10.1 (cities, 6127 places, 8.9 MB)
  Data generated from GeoNames (CC BY 4.0). Attribution is required in your app.
```

Progress output goes to **stderr** so stdout stays parseable — `terrakit add TR --json` emits only
JSON, which is what makes the CLI usable in scripts and CI.

### 7.1 Failure behaviour

| Failure | Behaviour |
|---|---|
| checksum mismatch | delete download, report, exit non-zero |
| signature invalid | delete, report keyId, **exit non-zero without fallback** |
| schema too new | refuse, tell the user which SDK version is needed |
| disk space insufficient | check before download, report required and available |
| network interrupted | resumable download via HTTP Range; partial files never installed |
| interrupted install | temp directory discarded, previous version untouched |
| concurrent install | `.lock` advisory lock, second process waits then proceeds |

No failure mode leaves a partially installed package visible to the SDK. Either the version
directory is complete and verified, or it does not exist.

---

## 8. Delta packages

Deferred past MVP per §33, but **designed now** because the identity scheme that makes deltas viable
is a day-one requirement ([`ARCHITECTURE.md` ADR-006](ARCHITECTURE.md#adr-006--row-level-deltas-not-file-level-or-block-level)).

```text
terrakit-data-tr-2026.10.1/
├── manifest.json
├── delta.sqlite        # operations, not a partial database
├── delta.sqlite.zst
└── signature.json
```

`delta.sqlite` holds explicit operations keyed by stable IDs:

```sql
CREATE TABLE delta_ops (
    seq         INTEGER PRIMARY KEY,
    op          TEXT NOT NULL CHECK (op IN ('insert','update','delete')),
    table_name  TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    payload     TEXT                    -- JSON object of changed columns; NULL for delete
);

CREATE TABLE delta_meta (
    key TEXT PRIMARY KEY, value TEXT NOT NULL
    -- base_version, target_version, operation_count, generatedAt
);
```

Operations reference IDs from [`DATA_MODEL.md` §3](DATA_MODEL.md#3-identity), never row offsets, so
a delta applies cleanly to a package several versions behind and unrelated changes elsewhere in the
dataset do not corrupt it.

Applied inside a single transaction by the SDK, which means failure leaves the existing package
untouched. `terrakit update` keeps a backup of `data.sqlite` before applying, making rollback a file
copy.

Section 11's worked example — 100 MB full versus 2.5 MB delta — is the target this design is
measured against once implemented. A delta that does not come in meaningfully under 10% of the full
package should be rejected in favour of a full download; a delta slower to apply than to re-download
is a regression, not an optimization.

---

## 9. `terrakit.json`

The project manifest from §10 of the specification.

```json
{
  "schemaVersion": 1,
  "packages": {
    "TR": "^2026.10.0",
    "DE": "^2026.9.0"
  },
  "level": "cities",
  "resolution": {
    "TR": { "version": "2026.10.1", "level": "cities", "integrity": "verified" }
  }
}
```

`packages` uses **semver ranges** over CalVer dataset versions, so `^2026.10.0` accepts
`2026.10.1` and rejects `2026.11.0`. `resolution` records what is actually installed. The split
mirrors `package-lock.json`: intent versus state.

The lockfile is committed to version control. Installed packages are shared, requested ranges are
per-project, and a team must get the same dataset on every machine — otherwise a bug reproduces on
one developer's machine and not on CI.

---

## 10. GitHub package sources

§11 requires `terrakit install github.com/example/terrakit-data-tr@1.2.0`.

```bash
terrakit install github.com/NexovaDev/terrakit-data-tr@1.2.0
terrakit install github.com/NexovaDev/terrakit-data-tr@main
terrakit install github.com/NexovaDev/terrakit-data-tr@a1b2c3d
```

Resolution:

| ref | resolution |
|---|---|
| `1.2.0` | semver tag |
| `@main` | branch HEAD, recorded as the resolved commit SHA |
| `@a1b2c3d` | commit |
| bare | repository default branch, warning that the result is not reproducible |

A branch or commit reference resolves to a concrete commit SHA, which is then pinned in
`terrakit.json` and recorded in the manifest as `resolvedRef`. Installing from `main` twice may
produce different bytes, so the CLI states the resolved SHA and writes it to the lockfile.

**The repository is treated as an untrusted transport, not as a trusted publisher.** Signature
verification applies identically. A GitHub repository with a valid TerraKit signature installs; one
without a trusted signature does not. `--allow-unsigned` exists for local development and prints a
prominent warning on every use, and refuses to run when CI is detected. If anyone can push to a
branch, anyone can push data, and the only thing standing between that and a production app is
signature verification.

Integrity also covers the clone itself: the CLI verifies the commit SHA it resolved against the one
it actually checked out, and verifies the extracted package's SHA-256 against its own manifest. A
manifest cannot claim a checksum the files do not have.

---

## 11. Registry

Static files. No database, no application server.

```text
https://registry.terrakit.dev/
├── index.json                        # all packages, latest versions
├── packages/terrakit-data-tr.json    # all versions of one package
└── packages/terrakit-data-tr/2026.10.1/manifest.json
```

```json
// index.json
{
  "schemaVersion": 1,
  "generatedAt": "2026-10-05T00:00:00Z",
  "packages": [
    {
      "name": "terrakit-data-tr",
      "version": "2026.10.1",
      "country": "TR",
      "level": "cities",
      "schemaVersion": 1,
      "size": { "raw": 48234496, "compressed": 8934212 },
      "entities": { "places": 6127, "cities": 486, "postalCodes": 9079 },
      "license": "CC-BY-4.0",
      "manifestUrl": "https://registry.terrakit.dev/packages/terrakit-data-tr/2026.10.1/manifest.json"
    }
  ]
}
```

Endpoints from §12 map onto these files:

| §12 endpoint | Implementation |
|---|---|
| `GET /packages` | `index.json` |
| `GET /packages/{name}` | `packages/{name}.json` |
| `GET /packages/{name}/versions` | `packages/{name}.json` → `versions[]` |
| `GET /packages/{name}/latest` | `packages/{name}.json` → `latest` |
| `GET /countries` | derived index, `packages[].country` |
| `GET /search` | static, served by the API; not a registry concern |

The static registry needs no authentication and no rate limiting for read paths, which is most of
§39's concerns removed before they exist. The dynamic API in [`API_SPEC.md`](API_SPEC.md) is only
needed when API keys and quotas appear (§37), and is a separate deployable reading from the same
object storage.

---

## 12. Reproducibility

§27 requires a reproducible pipeline. §27's guarantee, restated as a testable property:

> The same input dataset at the same version produces a byte-identical package.

Conditions the pipeline must satisfy:

| Requirement | Implementation |
|---|---|
| deterministic timestamps | `generatedAt` from `SOURCE_DATE_EPOCH`, never wall clock |
| deterministic row order | every insert sorted by a total order, `id` last as tiebreaker |
| fixed SQLite layout | fixed page size, `PRAGMA journal_mode=DELETE`, vacuum before close |
| deterministic compression | fixed zstd level and parameters |
| deterministic FTS5 build | rows inserted in sorted order; FTS5 output depends on insertion order |
| deterministic checksums | SHA-256 over the finished artifacts |

CI re-runs the pipeline against a pinned fixture and compares `packageSha256` against the committed
manifest. Any drift fails the build. Without this, "reproducible" is a claim rather than a property,
and §27's requirement is unmet.

---

## 13. Compatibility rules

Summary of every version number in the system and what may change without a bump.

| Version | Bump when | Reader tolerance |
|---|---|---|
| `packageFormatVersion` | layout or compression changes | older installers must refuse clearly, not misparse |
| `schemaVersion` | column or table added/removed/renamed | SDK checks against `minReaderVersion`/`maxReaderVersion` |
| SDK version | public API changes | — |
| `datasetVersion` | any data change, including renames | — |

Rules:

- Adding a table or a nullable column does **not** bump `schemaVersion`. Readers ignore what they do
  not know.
- Unknown manifest fields are ignored, never rejected. An SDK older than a package must still read
  it — this is the mechanism that keeps §31's independent versioning real rather than aspirational.
- Unknown `features` values cause a capability to be reported unsupported, never a crash.
- A package must be readable by the SDK version named in `minSdk` for **core** data. `minSdk` gates
  optional features only.
- `packageFormatVersion` is never silently upgraded by the installer.

---

## 14. Related documents

| Document | Contents |
|---|---|
| [`DATA_MODEL.md`](DATA_MODEL.md) | SQLite schema, ID grammar, normalization |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Why this format, ADR-006 on deltas |
| [`CLI_SPEC.md`](CLI_SPEC.md) | Commands, flags, exit codes, output contract |
| [`SECURITY.md`](SECURITY.md) | Key management, rotation, revocation, threat model |
| [`LICENSING.md`](LICENSING.md) | Per-source obligations, `attribution` requirements |
