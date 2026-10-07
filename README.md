# TerraKit

**The geographic data layer for developers.**

Install structured geographic data for any country as a verified, versioned, offline-first package —
then query countries, provinces, districts, cities, neighborhoods, postal codes, coordinates, time
zones, currencies and languages directly from your application.

```bash
terrakit init
terrakit add TR
```

```swift
import TerraKit

let antalya = Terra.city("Antalya")
print(antalya.name)        // Antalya
print(antalya.districts)   // [Muratpaşa, Kepez, ...]
print(antalya.timezone)    // Europe/Istanbul
```

```kotlin
import com.terrakit.core.Terra

val antalya = Terra.city("Antalya")
println(antalya.name)
println(antalya.districts)
println(antalya.timezone)
```

No API key. No network call. No geographic database to build and maintain.

---

## Status

**Pre-MVP. Architecture defined, implementation not started.**

| Area | State |
|---|---|
| Architecture, data model, package format | defined, under review |
| Normalization spec | normative, with conformance vectors |
| ADR-001 … ADR-007 | accepted |
| SQLite schema | v1, applied and exercised by every package |
| Data pipeline | **working** — builds TR, GE, DE, JP from live GeoNames data |
| Package validation | **working** — §30 rules, zero errors on all four |
| Reproducible builds | **verified** — byte-identical across runs |
| Localized / exonym / historic names | **working** — 19.2 M upstream rows, streamed |
| Rust CLI | **working** — query commands plus `add`, `remove`, `update`, `cache`, `bundle` |
| Registry | **working** — static `index.json` on object storage, Ed25519-signed releases |
| Swift SDK | specified ([`SDK_SPEC.md`](SDK_SPEC.md)), not built |
| Kotlin SDK | specified, not built |
| Registry | static layout specified, not hosted |

### Packages built from live data

```bash
python3 -m terrakit_pipeline build TR --level postal
```

| Country | Places | Postal codes | Localized names | Languages | zstd |
|---|---|---|---|---|---|
| TR | 5 091 | 2 953 | 20 769 | 245 | 1.6 MB |
| DE | 11 957 | 10 813 | 49 305 | 289 | 3.9 MB |
| JP | 3 428 | 142 577 | 15 455 | 320 | 3.9 MB |
| GE | 315 | none published | 3 271 | 264 | 0.2 MB |

Search now resolves historical and localized names, which is what SPEC §7 and §8 ask for:

```text
Terra.search("Constantinople")   ->  Istanbul   (class = historic)
Terra.search("Byzantion")        ->  Istanbul   (German, historic)
Terra.search("イスタンブール")      ->  Istanbul
Terra.localizedName(for: .japanese, place) -> "イスタンブール"
Terra.country("TR").name         ->  "Türkiye"  (not "Turkey")
```

```bash
./scripts/test.sh                          # 7 suites, all passing
./scripts/check_reproducible.sh TR         # byte-identical rebuild
```

### CLI

The read-only commands are complete and query real packages:

```console
$ terrakit search Constantinople
  CITY       Istanbul                 Istanbul, TR                 15 701 602

  1 result(s) in 2 ms

$ terrakit search イスタンブール
  PROVINCE   Istanbul                 Türkiye, TR                  15 701 602
  CITY       Istanbul                 Istanbul, TR                 15 701 602

$ terrakit info TR
Türkiye (TR/TUR /792)
    Capital      Ankara
    Driving      right
    Languages    av, az, diq, ku, tr
    Names        20769 localized in 245 languages, 870 alternates

$ terrakit list
  CC    LEVEL         VERSION              SIZE  INTEGRITY
  DE    postal        2026.10.1+a6c3    23.3 MB  verified
  TR    postal        2026.10.1+9920     9.9 MB  verified
```

A single tampered byte is caught, and `validate` exits `3` rather than `1` so a CI script can tell
"the package is not trustworthy" from "something broke":

```console
$ terrakit validate TR
✗ TR (2026.10.1+9920) — modified
  ✗ integrity: modified (modified)
    errors: 1, warnings: 1
$ echo $?
3
```

### Installing

`add` resolves the version, verifies the signature **before** fetching any bytes, checks free space,
downloads with resume, and installs by `rename` — so a package is either absent or complete:

```console
$ terrakit add TR GE
  resolving terrakit-data-tr from https://registry.terrakit.dev
    signature verified (keyId=terrakit-release-2026)
    downloading 1.7 MB
    downloaded 1.7 MB (1 attempt(s))
    installed 9.9 MB (.terrakit/packages/TR/2026.10.1-9920)
  resolving terrakit-data-ge from https://registry.terrakit.dev
    signature verified (keyId=terrakit-release-2026)
    installed  GE 2026.10.1+e83e (cities, 1.3 MB, signed by terrakit-release-2026)
```

A verification failure is exit `3`, distinct from a runtime failure, so a CI script can refuse to
trust the result without treating a flaky network as an attack:

```console
$ terrakit add TR; echo $?
terrakit: package verification failed.
       signed by key "rogue-2026", which is not trusted.
       Keys consulted: terrakit-release-2026 [built-in].
       To trust it, add [[keys]] with that id and publicKey to ~/.config/terrakit/config.toml.
       Verify the public key through a channel independent of this download before you do —
       a key delivered alongside the package it signs proves nothing.
3
```

`file://` is a supported registry transport, which is what makes mirrors, air-gapped installs and
this project's own tests possible without a network.

### Registry

Static files, no server — `aws s3 sync` or `rclone sync` is the whole deployment. Published
versions are immutable, the index is byte-reproducible, and URLs are relative so a copy of the
tree is read from the copy:

```bash
# build and sign a release
PYTHONPATH=data-pipeline/src python3 -m terrakit_pipeline publish datasets/terrakit-data-tr-* \
  --sign-key /path/to/release-key.pem --sign-key-id terrakit-release-2026

# publish the tree
tools/build_registry.py datasets/ --out registry/ \
  --base-url https://registry.terrakit.dev \
  --upload aws:s3://my-bucket/terrakit --production
```

`--production` refuses anything signed with the committed development key. That guarantee is
tested, not just documented: [`scripts/test_registry.sh`](scripts/test_registry.sh) is 90 checks
covering registry install, update, remove, cache listing, verification and pruning, mobile bundling,
GitHub install, hostile archives, unsigned packages, damage detection and repair, idempotency,
relocatability, reproducibility, the refusals, and the `--json` contract.

### Removing and updating

```console
$ terrakit update --check
  GE           2026.10.1+e83e -> 2026.10.2+aaaa update available

$ terrakit update
  updating GE: 2026.10.1+e83e -> 2026.10.2+aaaa
  GE           2026.10.1+e83e -> 2026.10.2+aaaa updated  (freed 1.3 MB)

$ terrakit remove GE
  removed  GE 2026.10.2+aaaa (cities, 1.3 MB, was verified)
  freed 1.3 MB
```

`update` stays inside the project's pin and removes the version it replaced — leaving two
versions of one country installed would make `search` return the same places twice with identical
IDs, and a caller cannot tell a duplicate from a real second result. `remove` leaves the shared
download cache alone unless `--prune` is given, because one project's `remove` should not turn
another project's install into a download.

A package whose bytes no longer match its manifest is reported, not treated as installed:

```console
$ terrakit add GE
terrakit: GE@2026.10.2+aaaa is installed but modified.
       Nothing has been changed. Re-run with --force to replace it.
$ echo $?
3
```

### Cache and mobile bundles

```console
$ terrakit cache list --cache-dir ~/.cache/terrakit
  DIGEST                              SIZE    AGE  STATE
  5fe6db5509f4da40               261.0 KB     9s  in use

$ terrakit cache verify
1 cache entry verified, 261.0 KB

$ terrakit bundle --platform all
bundled 1 package(s) for ios, android
  6 file(s), 2.7 MB
```

The cache is shared across projects, so `cache clean` only removes entries nothing installed
references *and* that are older than a week — an entry another project installed a minute ago is
unreferenced as far as this project can tell. `cache verify` re-hashes every entry and deletes the
ones that fail, because an entry that fails its own digest has no value.

`bundle` closes the gap [`ARCHITECTURE.md` §6](ARCHITECTURE.md#6-mobile-packaging) names:
`terrakit add` writes to `.terrakit/`, but an iOS or Android app reads only from its bundle. The
output is declarative and checked in, so CI needs no network:

```text
bundle/
├── terrakit-bundle.json          canonical manifest, platform-independent
├── ios/terrakit/<CC>/<version>/  add as an Xcode folder reference
└── android/
    ├── assets/terrakit/<CC>/<version>/   Gradle ships assets/ verbatim
    └── terrakuit.gradle          noCompress snippet, included by the app
```

### Installing from a GitHub source

```console
$ terrakit install github.com/example/terrakit-data-tr@main
  resolving example-terrakit-data-tr
    main -> 4f2a1c8e9b3d5a7f0c1e2d3b4a5f6e7d8c9b0a1b
    note: main is not a versioned ref. Installing what it points at right now;
          the resolved commit is pinned in terrakit.json so this stays reproducible.
    signature verified (keyId=terrakit-release-2026)
    file digests match the manifest
  installed  TR 2026.10.1+9920 (postal, 9.9 MB, signed by terrakit-release-2026)
```

A GitHub repository is an untrusted transport, not a trusted publisher: anyone who can push to a
branch can push data, so signature verification applies exactly as it does for a registry package.
Tarball entries are treated as hostile — `..`, absolute paths, symlinks, hardlinks, entries under
two roots, and decompression bombs are all refused, and each exits `3` rather than `1` so a CI
script does not mistake an attack for a flaky network. `--allow-unsigned` exists for local
development and refuses to run in CI.

Read the specification documents before contributing. They are the source of truth, and code that
disagrees with them is a bug.

---

## Documentation

| Document | Purpose |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | System design and architectural decisions |
| [`DATA_MODEL.md`](DATA_MODEL.md) | Entities, identity, normalization, SQLite schema |
| [`PACKAGE_FORMAT.md`](PACKAGE_FORMAT.md) | Package layout, manifests, verification, deltas |
| [`core/spec/normalization.md`](core/spec/normalization.md) | Normative query normalization algorithm |
| [`LICENSING.md`](LICENSING.md) | Source licenses and attribution obligations |
| [`SECURITY.md`](SECURITY.md) | Threat model and signature scheme |
| [`data-pipeline/sources/README.md`](data-pipeline/sources/README.md) | Pinned sources and known upstream gaps |
| [`CLI_SPEC.md`](CLI_SPEC.md) | Command surface, exit codes, stdout/stderr contract |
| [`SDK_SPEC.md`](SDK_SPEC.md) | Swift and Kotlin public API |
| [`DATA_PIPELINE.md`](DATA_PIPELINE.md) | Ingestion stages and pipeline decisions |
| [`API_SPEC.md`](API_SPEC.md) | Hosted API *(stub — optional, never on the SDK's path)* |
| [`docs/adr/`](docs/adr/) | Architecture decision records |

---

## Design commitments

These are enforced, not aspirational. Each links to the decision that makes it real.

1. **Offline-first.** The SDK opens a read-only SQLite file. It is the only data source it has, and no
   query path has a network fallback branch.
   ([§5](ARCHITECTURE.md#5-offline-first-contract))
2. **Stable identity.** IDs come from ISO 3166-1, ISO 3166-2 and GeoNames — never from names. They
   survive renames, transliteration changes and administrative reorganization.
   ([ADR-002](docs/adr/ADR-002-identity-from-external-authorities.md))
3. **No fixed hierarchy.** One `places` table with `type` (semantic) and `level` (structural) handles
   Türkiye, the United States, Germany and Japan without schema changes.
   ([ADR-001](docs/adr/ADR-001-unified-places-table.md))
4. **Compute once, at build time.** Normalization, deduplication, hierarchy construction, validation
   and indexing all happen in the pipeline. SDKs are query engines.
   ([§1](ARCHITECTURE.md#1-architecture-in-one-paragraph))
5. **Never load a dataset into memory.** Indexed queries, keyset pagination, hard result caps, and an
   enforced memory ceiling in CI. (§41)
6. **Verifiable by construction.** SHA-256 plus Ed25519 signatures, verified before extraction and
   again after, with atomic install and fail-closed behaviour.
   ([§5](PACKAGE_FORMAT.md#5-integrity))
7. **Attribution never hidden.** License is declared per package and per source, the build fails
   without a resolved license, and SDKs expose `Terra.attribution`.
   ([ADR-004](docs/adr/ADR-004-geonames-first-osm-later.md))
8. **Reproducible builds.** Same input, byte-identical output, verified in CI.
   ([§11](ARCHITECTURE.md#11-reproducibility))
9. **Independent versioning.** SDK, package format, schema and dataset version separately.
   ([§13](PACKAGE_FORMAT.md#13-compatibility-rules))
10. **Cross-language determinism.** Python, Rust, Swift and Kotlin produce identical results from
    identical bytes, verified against shared golden vectors.
    ([§8](ARCHITECTURE.md#8-cross-language-determinism))

---

## MVP scope

**Countries:** Türkiye, Georgia, Germany, United States, United Kingdom, plus Japan in the test set
because its address structure differs substantially from Western models.

**Levels:** `basic`, `administrative`, `cities`, `postal`. Streets are excluded — see below.

**Tools:** CLI, Swift SDK, Kotlin SDK, registry, SQLite packages, search, autocomplete, validation,
versioning.

### Deliberately out of scope

Streets are the notable exclusion. Street data requires OpenStreetMap extraction, and OSM's ODbL
license imposes share-alike obligations on derived databases that would constrain every downstream
commercial user. Adopting it later is a data change, not a format change: the schema already reserves
level 7 and the `street` ID namespace.
([ADR-004](docs/adr/ADR-004-geonames-first-osm-later.md))

Also deferred, per the specification: delta packages (designed,
[ADR-006](docs/adr/ADR-006-row-level-deltas.md)), hosted API and dashboard, geocoding and routing,
and SDKs beyond Swift and Kotlin.

### Known gaps in the current pipeline

- **Short CJK queries.** The FTS5 trigram tokenizer needs three characters, so `search("東京")` finds
  nothing while `search("東京都")` works. CJK names are short enough that this bites harder than it
  does in Latin script; the reason it is not "fixed" is in
  [`DATA_MODEL.md` §9](DATA_MODEL.md#9-search-schema).
- **No boundaries and no geocoding**, per spec §46.

Upstream data defects encountered while building the MVP countries — Georgia having no postal
file, `admin1_code` of `00`, one Turkish settlement with the timezone `Asia/Baghdad` — are each
recorded with the handling in
[`data-pipeline/sources/README.md`](data-pipeline/sources/README.md).

---

## Data sources

| Source | License | Used for |
|---|---|---|
| [GeoNames](https://www.geonames.org/) | CC BY 4.0 | countries, administrative areas, settlements, postal codes |
| ISO 3166-1 / 3166-2 | freely available | country codes, stable administrative identifiers |
| ISO 4217 | freely available | currency codes |
| IANA Time Zone Database | public domain | timezone identifiers |
| OpenStreetMap | ODbL 1.0 | planned, streets and neighborhoods |

Every package records `source`, `sourceVersion`, `license`, `attribution` and `retrievedAt`. The
pipeline refuses to build a package whose source has no resolved license.

---

## Contributing

The specification requires identifying contradictions and missing requirements before writing
implementation code. That work is in progress, and the architecture documents are open for review.

Start with [`ARCHITECTURE.md` §13](ARCHITECTURE.md#13-open-questions) — the five open questions are
where judgement is needed and where a contribution has the most leverage.

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers setup, the documentation-first workflow and the
conformance test requirements for anything touching normalization or search.

---

## License

Code: see [`LICENSE`](LICENSE).
Data packages: **CC BY 4.0**, per source, declared in each package manifest. Some future packages
derived from OpenStreetMap will be **ODbL 1.0**. Attribution requirements are documented in
[`LICENSING.md`](LICENSING.md) and are never optional.
