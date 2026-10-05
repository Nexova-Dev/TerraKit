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
| Localized / alternate names | schema ready, ingestion not written |
| Rust CLI | specified ([`CLI_SPEC.md`](CLI_SPEC.md)), not built |
| Swift SDK | specified ([`SDK_SPEC.md`](SDK_SPEC.md)), not built |
| Kotlin SDK | specified, not built |
| Registry | static layout specified, not hosted |

### Packages built from live data

```bash
python3 -m terrakit_pipeline build TR --level postal
```

| Country | Places | Postal codes | Package |
|---|---|---|---|
| TR | 5 091 | 2 953 | 6.0 MB |
| DE | 11 957 | 10 813 | 13 MB |
| JP | 3 428 | 142 577 | 53 MB |
| GE | 315 | none published | 0.5 MB |

```bash
./scripts/test.sh                          # 4 suites, all passing
./scripts/check_reproducible.sh TR         # byte-identical rebuild
```

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

- **Localized names are not ingested.** `place_names` and `place_alternate_names` exist in the
  schema and are validated, but `alternateNamesV2.zip` is not read yet, so search relies on
  `places.name` alone. `Terra.search("Constantinople")` will not reach İstanbul until it is. This
  is the next pipeline stage and the largest remaining gap.
- **Country names are English.** GeoNames publishes `Turkey`; the endonym `Türkiye` needs
  `alternateNames`.
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
