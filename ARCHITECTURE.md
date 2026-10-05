# TerraKit Architecture

Status: **Draft / Accepted for MVP**
Last updated: 2026-10-05
Scope: architecture decisions that constrain every other component.

This document explains *why* TerraKit is built the way it is. Concrete schemas live in
[`DATA_MODEL.md`](DATA_MODEL.md), concrete file layouts in [`PACKAGE_FORMAT.md`](PACKAGE_FORMAT.md).

---

## 1. Architecture in one paragraph

TerraKit is a **build-time compiler and a runtime query engine**. A Python pipeline ingests
licensed raw datasets into a PostGIS master database, normalizes and validates them, and emits
immutable, checksummed SQLite packages. The Rust CLI fetches and installs those packages.
The Swift and Kotlin SDKs never download, never parse, and never generate data — they open a
read-only SQLite file and answer indexed queries. Everything except package acquisition is
offline, and no code path loads a whole dataset into memory.

```
                 BUILD TIME (online, heavy)                RUN TIME (offline, fast)
 ┌──────────────────────────────────────────┐        ┌────────────────────────────────────┐
 │ Raw sources (GeoNames CC-BY, OSM later)  │        │  App                                │
 │            ↓ Downloader                  │        │    ↓ import TerraKit               │
 │            ↓ Parser                      │        │  SDK ── read-only SQLite ──┐       │
 │            ↓ Normalizer (NFKD + fold)    │        │   ├─ exact / prefix       │       │
 │            ↓ Deduplicator                │        │   ├─ trigram FTS5         │       │
 │            ↓ Hierarchy builder           │        │   ├─ phonetic             │       │
 │            ↓ Validator ── reject on error│        │   └─ R*Tree nearest       │       │
 │            ↓ Package builder             │        │                           │       │
 │            ↓ SHA-256 + sign             │        │                           ↓       │
 └──────────────┬───────────────────────────┘        │                    .terrakit/    │
                ↓                                    │                    or app bundle │
        PostGIS master DB                           └────────────────────────────────────┘
                ↓
        registry (index.json + manifests on CDN)
                ↓
        Rust CLI: terrakit add TR
```

The single most important property: **normalization, deduplication, hierarchy construction,
validation and indexing happen once, at build time.** The SDK is a query engine, not a data
processor. This is what makes the performance targets in §40 of the specification reachable and
what keeps the SDKs small enough to trust.

---

## 2. Repository layout

```text
terrakit/
├── core/
│   ├── spec/          # normative, cross-language specs (this directory's siblings)
│   └── schema/        # SQLite DDL shipped in packages, one file per schema version
├── cli/               # Rust — standalone binary, zero runtime deps beyond libc + libsqlite3
├── data-pipeline/     # Python — ingestion, normalization, validation, package emission
├── database/
│   ├── migrations/    # PostGIS migrations, forward-only
│   └── seed/          # ISO 3166, IANA, ISO 4217 static reference data
├── registry/
│   ├── schema/        # registry manifest JSON Schema
│   └── public/        # static index served over CDN
├── api/               # TypeScript — optional hosted API (never required by SDKs)
├── sdk/
│   ├── swift/         # SPM package
│   └── kotlin/        # Gradle, multiplatform (JVM + Android)
├── docs/              # user documentation, ADRs
├── examples/
├── tests/
│   ├── fixtures/      # shared golden datasets, identical bytes for every language
│   └── integration/   # cross-SDK conformance suites
├── datasets/          # generated packages, git-ignored except manifests
├── docker/
└── tools/
```

### Monorepo, five independently versioned artifacts

| Artifact | Language | Versioned independently? |
|---|---|---|
| `terrakit` CLI | Rust | yes |
| TerraKit Swift | Swift | yes |
| `geo-core` Kotlin | Kotlin | yes |
| Package **format** | — | yes, `packageFormatVersion` |
| Geographic **dataset** | — | yes, `2026.10.1` |

Per §31 of the specification, software and data versions must never be coupled. A dataset
release must not force an SDK release, and an SDK release must not invalidate installed data.
Concretely: an SDK supports any package whose `packageFormatVersion` falls in its declared range,
and a package declares a minimum SDK version for optional features only — never for the core.

---

## 3. Key decisions

Each is recorded as an ADR in `docs/adr/`. Summaries below.

### ADR-001 — One `places` table, not one table per entity type

**Rejected:** the schema in §13 of the specification — `regions`, `states`, `provinces`,
`counties`, `districts`, `neighborhoods` as separate tables.

**Accepted:** a single `places` table with a `type` discriminator, plus a closure table for
ancestry.

The specification contradicts itself: §13 prescribes one table per level while §4, §48 and §56.7
insist that not every country shares a hierarchy, and §52 deliberately includes Japan because its
structure "can differ substantially from Western models". Japan has no county layer, Germany has
no stable street-level postal mapping, the United States has three distinct area types above the
city. Per-type tables do not fail gracefully on those cases — they accumulate nullable columns
and empty tables, and every query becomes a `UNION`.

With `places(type, parent_id)`:
- any depth from 1 to 7 works with no schema change;
- adding a new area type is a data change, not a migration;
- "all descendants of X" is one indexed join against `place_closure`;
- traversal in Swift/Kotlin/Rust is one function instead of seven.

`type` (semantic: `city`, `town`, `village`, `district`, `neighborhood`) and `level` (structural
depth: 1–7) are separate columns. Germany and Japan legitimately have a `district` at different
depths; conflating them is how address libraries break.

Cost accepted: no foreign-key-level type safety between parent and child. Enforced by the
validator in the pipeline and by a CHECK constraint on the level transition table, not by the
database.

### ADR-002 — Identity is an external code, not a name

**Rejected:** §6's `city_tr_antalya`. It breaks on rename (Türkiye, Istanbul, Chennai), on
collision (two distinct settlements sharing a name in one province), and on transliteration
changes. Every rename would orphan downstream references — which is exactly what §6 forbids.

**Accepted:** namespaced identifiers derived from external authorities, in priority order:

1. **ISO 3166-1** for countries (`country:TR`), **ISO 3166-2** for administrative areas
   (`admin:TR-07`, `admin:JP-27`, `admin:DE-BE`, `admin:US-CA`) — codes are stable across
   renames by design, which is their entire purpose.
2. **GeoNames** (`place:geonames:3054643`) for settlements and administrative subdivisions below
   level 2, where ISO codes stop.
3. **OSM** (`street:osm:way:1234567`) for streets, when OSM enters the pipeline.
4. **Deterministic fallback hash** over `country + type + normalized name + admin path`, used only
   when no external identifier exists. Documented as weaker: a rename creates a new ID.

The full grammar, fallback rules and migration policy are in
[`DATA_MODEL.md` §3](DATA_MODEL.md#3-identity).

### ADR-003 — Search is three mechanisms, not one

The specification asks for exact, prefix, fuzzy, localized and typo-tolerant search (§16) plus
structured autocomplete (§17) under 50 ms and 100 ms respectively. One mechanism cannot do this.

Accepted decomposition:

| Need | Mechanism | Where it runs |
|---|---|---|
| Exact | `places.name_norm` equality, B-tree | build-time index |
| Prefix | `places.name_norm` `LIKE 'x%'`, B-tree | build-time index |
| Fuzzy / substring / infix | **FTS5 `trigram`** tokenizer | build-time index |
| Typo tolerance | precomputed **Double Metaphone** codes, B-tree | build-time index |
| Localized names | `place_names` joined, then normalized | build-time index |
| Ranking | integer `search_rank` column, precomputed | build-time |

Two consequences worth stating plainly:

- **Phonetic codes and normalized names are computed by the pipeline and shipped in the package.**
  SDKs never compute phonetic codes. Implementing Double Metaphone correctly in Swift, Kotlin and
  Rust and then hoping all three agree byte-for-byte is an ongoing liability; a build-time table
  makes agreement a data problem that CI can verify once.
- The **only** normalization an SDK performs is normalizing the user's query string. That is a
  small, precisely specified function (§8 below) that ships in all three SDKs and is verified
  against a shared golden vector file in `tests/fixtures/normalization.json`.

Verified on the target platform: SQLite 3.54 with `ENABLE_FTS5` and `ENABLE_RTREE` available.
FTS5 trigram requires SQLite ≥ 3.34; that is our minimum floor everywhere, Android included.

### ADR-004 — GeoNames CC-BY first, OSM ODbL later

Per the licensing decision recorded in §29 and §47 of the specification.

GeoNames is CC-BY 4.0: it permits redistribution, modification and commercial use, and requires
attribution. This keeps TerraKit's packages under a permissive license and avoids the ODbL
share-alike obligation that would attach to any derived database.

OSM remains the correct long-term source for streets and neighborhoods, but adopting it changes the
license of any package that contains OSM-derived data. Therefore:
- packages declare `license` and `sources[]` per package, never globally;
- every source record carries `license`, `attribution`, `sourceVersion`, `url`;
- the pipeline refuses to emit a package whose source has no resolved license — it fails the
  build rather than shipping unattributable data (§29: "must never hide source attribution");
- SDKs expose `Terra.attribution` so an application can satisfy its own obligations without
  reading our documentation.

This is why OSM streets are explicitly out of MVP scope. Street data is where ODbL becomes
unavoidable, and mixing license regimes inside a single package format is a design mistake to
avoid now rather than unwind later.

### ADR-005 — PostGIS, not plain PostgreSQL

§43 recommends PostgreSQL; §19 requires nearest-location queries and spatial indexes. PostGIS is
PostgreSQL plus the spatial types and GiST/SpTree indexes that §19 and §40 require. There is no
version of this that is plain PostgreSQL.

`nearest` in the master database uses `ST_DWithin` against a geometry column with an SpGist index.
PostGIS is used in the pipeline only. Runtime packages use SQLite R*Tree (§4), which keeps the
SDK dependency-free.

### ADR-006 — Row-level deltas, not file-level or block-level

§33 wants 100 MB full versus 2.5 MB delta. Three options were compared:

| Scheme | Delta size | Complexity | SDK risk |
|---|---|---|---|
| Block-level binary diff | small-ish | high, fragile | high, corruption cascades |
| Whole-file shard replace | large, depends on shard count | low | low |
| **Row-level operation log** | **small, predictable** | **medium** | **low** |

Accepted: **row-level deltas.** The pipeline emits a `delta.sqlite` containing explicit
`insert` / `update` / `delete` operations against stable IDs from ADR-002, plus a manifest of which
package version it applies to. The SDK replays it inside a single transaction and can roll back.

Because operations reference stable IDs rather than row offsets, a delta survives unrelated changes
elsewhere in the dataset and applies cleanly to a package several versions behind. Block-level
diffs do not have that property. Deltas ship as `deltaPackage` in `PACKAGE_FORMAT.md` and are
implemented after the MVP, as §33 specifies — but the ID scheme that makes them possible is a
day-one requirement, not a later optimization. This is the reason ADR-002 cannot be deferred.

### ADR-007 — Rust CLI, Python pipeline, TypeScript API

Per §43. Rust for the CLI because it is distributed as a single static binary with a
libsqlite3 dependency and no runtime, and because SHA-256 verification must not be
inconsistent. Python for the pipeline because the geospatial data ecosystem is Python and the
pipeline is developer-facing, not shipped. TypeScript for the API, which is optional hosted
infrastructure.

The registry is **static files**. `index.json` plus per-package manifests on object storage behind
a CDN serves every read path the CLI needs. No database, no application server. A dynamic API is
only required when API keys and rate limiting appear (§37), and that is a separate deployable.

---

## 4. Runtime query strategy

The performance targets (§40) are met by never asking the wrong question of SQLite.

**Nearest location** uses an SQLite **R\*Tree** virtual table, not a full scan with a bounding-box
prefilter. The bounding-box prefilter is the obvious implementation and it is wrong: at high
latitudes a 1 km box filters almost nothing, and the scan degrades exactly where databases are
densest — Europe, Japan, the US northeast.

**Hanging ancestry** (`Antalya → Muratpaşa → Antalya Province → Türkiye`) is a single indexed
join against `place_closure(ancestor_id, descendant_id, depth)`. No recursive CTE, no per-level
query, no depth limit guesswork.

**Parent → children** is `places(parent_id)` with a composite index `(parent_id, type, name_norm)`.

**Pagination** is always keyset, never `OFFSET`. `OFFSET 500000` in SQLite walks 500 000 rows and
discards them; street lists and large administrative subdivisions hit this immediately.

**Search ranking** is a precomputed integer `search_rank` per row, set by the pipeline from
population, capital status and level. Ranking in SQL at query time cannot use an index. Sorting
by a precomputed integer column is a direct index scan.

**Memory ceiling.** No public API returns an unbounded collection. Every list-returning method takes
a limit; the SDK enforces a hard default cap (1000 for entities, 50 for autocomplete) and never
materializes a full table scan into a Swift `Array` or Kotlin `List`.

---

## 5. Offline-first contract

§15 makes offline operation a core feature, not a fallback. The architectural guarantee:

- the SDK opens SQLite **read-only**, and it is the only data source it has;
- the only network-touching code in the SDK is `updateAvailable()`, which is explicitly opt-in and
  returns a value rather than performing an install;
- no query path has a network fallback branch. If the package is not installed, the query returns
  `nil` or throws `.packageNotInstalled` — it never silently reaches out;
- `terra doctor` reports which packages are installed, their versions, integrity status and
  available disk space, without mutating anything.

A useful consequence: the SDK has no network permissions requirement to be useful, and the iOS
build has no `NSAppTransportSecurity` exceptions.

---

## 6. Mobile packaging

A gap in the specification worth naming: `terrakit add TR` writes to `.terrakit/`, but an iOS or
Android app reads only from its bundle. Without an explicit step, the promised experience in §57
does not work on mobile.

Accepted design:

1. `terrakit add TR` installs into `.terrakit/packages/TR/<version>/` and writes
   `terrakit.json`.
2. `terrakit bundle --platform ios` generates an Xcode resource manifest and, for Gradle, copies
   or registers packages as assets. Both are declarative and checked into the repo — no build-time
   network access, so CI stays hermetic.
3. At runtime the SDK resolves packages in order: explicit configuration, then the app bundle,
   then `.terrakit/`. First match wins. Resolution is one `stat`, done once and cached.

Android deserves an explicit note: on Android the SQLite file is copied from assets into the app's
private directory on first launch, because a file inside an APK asset stream is not a seekable
file and SQLite requires `mmap` and random access. This copy is performed once per package version
and skipped thereafter. Bundling the database uncompressed in the APK, or shipping it in
`assets/` with `noCompress`, avoids inflating it twice.

---

## 7. Module boundaries

Layered, one direction only. `core/spec` is normative and depended upon by everything; nothing
depends on an SDK.

```
  cli ──────────────┐
                    ├──▶ core/spec (normative documents + shared test vectors)
  sdk/swift ────────┤
  sdk/kotlin ───────┘
        │
        └──▶ core/schema (SQLite DDL, versioned) ──▶ packages ──▶ devices

  data-pipeline ──▶ database (PostGIS) ──▶ packages
  api ────────────▶ packages (read-only)
  registry ───────▶ packages (metadata only)
```

Enforced rules:
- the CLI never writes data content into a package, it only fetches, verifies and installs;
- the API never writes data, it is read-only over published packages;
- SDKs never depend on each other or on the CLI;
- no component executes code from a package. Packages are data plus metadata (§39).

---

## 8. Cross-language determinism

Four runtimes touch the same bytes: Python pipeline, Rust CLI, Swift SDK, Kotlin SDK. Three
things must be identical everywhere or the SDKs will return different results for the same query.

1. **Query normalization** — implemented in all three SDKs from one normative spec, verified in CI
   against `tests/fixtures/normalization.json`. This is a ~120-line function; the spec, not the
   code, is the source of truth.
2. **Database contents** — produced only by the pipeline, shipped as bytes. Never regenerated by
   an SDK. This is why §56.8 says "separate data from SDK logic".
3. **JSON decoding of manifests** — manifest parsing is manifest-versioned, and unknown fields are
   ignored rather than rejected, so an SDK older than a package still reads it.

Unicode is the sharp edge here. Normalization is specified as **NFKD**, case mapping is specified
as **full Unicode case folding** rather than `lowercase()`, and there is a shared test vector set
covering Turkish dotted/dotless I (`İ`/`i`/`ı`), German ß, Georgian, Greek and Japanese. Folding
`I` to `i` is not the same as lowercasing in a Turkish locale, and getting it wrong makes
`Istanbul` unfindable — a headline example in §8 of the specification.

---

## 9. What is deliberately not built yet

Deferred per §33, §44, §45, §46, and the MVP boundary in §48:

| Deferred | Reason |
|---|---|
| Streets in packages | Requires OSM extraction, which changes licensing (ADR-004). Out of MVP scope. |
| Delta packages | Format designed (ADR-006), implementation after MVP. |
| Hosted API and dashboard | Optional infrastructure; static registry serves the core. |
| Geocoding, routing, boundaries | §46: not MVP. Separate modules, never in the core package. |
| SDKs beyond Swift/Kotlin | §44 requires a language-independent data model; it is. Others follow. |
| Currency exchange rates | §22 explicitly excludes this from the core. |

---

## 10. Performance verification

§40 asks for benchmarks, and §56 requires designing for millions of records. Neither is served by
assertions in a document, so the acceptance criteria are mechanical:

- `tests/integration/conformance/` runs the same query battery against the same package bytes in
  all SDKs and compares result ID sets, not just counts;
- `bench/` measures lookup, indexed search, autocomplete, fuzzy search, coordinate lookup, nearest
  location, database open and package install against a large fixture;
- a regression budget fails CI when p95 lookup exceeds 10 ms or autocomplete exceeds 100 ms on
  reference hardware, recorded in `docs/BENCHMARKS.md` with the machine spec;
- a memory ceiling test asserts peak RSS during a street-scale search stays under a documented
  bound, enforcing §41 mechanically rather than by convention.

---

## 11. Reproducibility

§27 requires a reproducible pipeline and §56.11 requires reproducible builds. A package is
reproducible when the same inputs produce byte-identical output.

To achieve that: all generated timestamps come from the pipeline run's `SOURCE_DATE_EPOCH`, not the
wall clock; SQLite files are built with a fixed page size and vacuumed so free pages do not vary;
rows are inserted in a deterministic sorted order; compression uses a fixed zstd level; and the
manifest's `generatedAt` is derived from the input dataset version. Re-running the pipeline against
unchanged GeoNames input must yield the same SHA-256. CI verifies this on every release build.

---

## 12. Related documents

| Document | Contents |
|---|---|
| [`DATA_MODEL.md`](DATA_MODEL.md) | Entity model, ID grammar, normalization algorithm, schema DDL |
| [`PACKAGE_FORMAT.md`](PACKAGE_FORMAT.md) | Package layout, manifest, checksums, deltas, install flow |
| [`CLI_SPEC.md`](CLI_SPEC.md) | Command surface, flags, exit codes, output contract |
| [`SDK_SPEC.md`](SDK_SPEC.md) | Swift and Kotlin public API surface |
| [`LICENSING.md`](LICENSING.md) | Source licenses, attribution obligations, redistribution |
| [`SECURITY.md`](SECURITY.md) | Threat model, signature scheme, key rotation |
| [`DATA_PIPELINE.md`](DATA_PIPELINE.md) | Ingestion stages, validation rules, failure policy |
| `docs/adr/` | One file per decision recorded in §3 |

---

## 13. Open questions

Deferred deliberately, with the condition that would resolve each:

1. **Japanese and Georgian transliteration.** GeoNames supplies transliterated alternate names, so
   cross-script search may need no in-house transliterator at all. Resolve after measuring
   recall on `GE` and `JP` fixtures; add a Cyrillic/Greek table only if recall is insufficient.
2. **Street-level postal mapping.** The US ZIP-to-street relation is genuinely one-to-many and large.
   Model as an extension table rather than compromising the MVP model. Decide when streets enter.
3. **Placeholder names.** GeoNames marks untranslated local-language names. Whether to ship them as
   `name` or suppress them affects search quality in TR/GE. Decide from fixture analysis.
4. **Registry hosting.** Object storage plus CDN is sufficient for the core; decide concrete
   provider when the API ships.
5. **Private and enterprise packages** (§47). Same format, different visibility. The format already
   supports it; authentication is the only missing piece.
