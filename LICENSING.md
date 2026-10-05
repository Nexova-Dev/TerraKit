# LICENSING

Status: **Draft**
Last updated: 2026-10-05

§29 of the specification makes licensing a first-class feature and states that the project "must
never hide source attribution". This document turns that into obligations an implementer can check.

---

## 1. Two separate licenses

TerraKit has two independently licensed artifacts. Conflating them is the most common way an
open-source data project gets its users into trouble.

| Artifact | License | Where declared |
|---|---|---|
| **Code** — CLI, SDKs, pipeline, registry, docs | MIT | `LICENSE` at repository root |
| **Data packages** — generated `.sqlite` artifacts | **per source**, never global | `manifest.license`, `manifest.sources[].license` |

The MIT license on the SDK does **not** grant any rights to the data. A user who accepts the MIT
license of the Swift SDK still owes GeoNames attribution for the package it reads. This is stated in
the README, the `LICENSE` file, every package manifest and the `Terra.attribution` API.

---

## 2. Source licenses

| Source | License | Redistribution | Share-alike | Attribution | MVP role |
|---|---|---|---|---|---|
| GeoNames | CC BY 4.0 | permitted, commercial | none | required | primary |
| ISO 3166-1 / 3166-2 | freely available | permitted | none | citation requested | primary |
| ISO 4217 | freely available | permitted | none | citation requested | reference |
| IANA tzdb | public domain | permitted, commercial | none | requested | reference |
| OpenStreetMap | ODbL 1.0 | permitted, commercial | **required** | required | planned |
| Government open data | varies | **must be checked per dataset** | varies | varies | not used yet |

**Government open data is not a category with one answer.** Postal registries in particular often
publish under terms that forbid commercial redistribution. §28 permits official datasets "where
licensing allows", which means the check is per dataset, not per country. The pipeline records a
resolved license for every source and refuses to emit a package without one.

---

## 3. ODbL: what it means and why streets are deferred

ODBL 1.0 is a copyleft license for databases. Two obligations matter for TerraKit:

**Share-alike.** A public database derived from OSM data must be released under ODbL or a
compatible license. This applies to the produced database, so a package containing OSM-derived data is
itself ODbL. It does **not** extend to code that merely reads the database — a Swift application
using an ODbL package is not forced to become ODbL.

**Attribution.** Any public use must carry the required attribution notice.

Consequences for the project:

1. Any package with OSM-derived data is **ODbL**, regardless of the project's code license.
2. Packages will therefore exist under **two different licenses**. Users must check
   `manifest.license` per package. This is disclosed, never hidden.
3. The obligation to make the derived database public is on whoever distributes a derived database,
   not on TerraKit. TerraKit distributes the source-derived package; a user's own augmented copy is
   that user's question. This document states the obligation so nobody discovers it late.
4. Adopting OSM mid-project would have imposed these terms on every existing package. Deferring
   keeps the MVP permissively licensed.
   ([`ADR-004`](docs/adr/ADR-004-geonames-first-osm-later.md))

---

## 4. Obligations per package

Every package manifest carries:

```json
{
  "license": "CC-BY-4.0",
  "sources": [
    {
      "id": "geonames",
      "version": "2026-09-01",
      "license": "CC-BY-4.0",
      "attribution": "GeoNames, CC BY 4.0",
      "url": "https://www.geonames.org/",
      "retrievedAt": "2026-09-28T12:00:00Z",
      "entityShare": { "countries": 1, "postalCodes": 9079, "places": 0 }
    }
  ],
  "attribution": ["Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0."]
}
```

`entityShare` exists because attribution under CC BY is per-work, and a package can mix sources. A
future package that is 90% GeoNames and 10% OSM must let a consumer attribute each part correctly.
Declaring one license for the whole package would be inaccurate in exactly the case where accuracy
matters.

---

## 5. Required CC BY attribution notice

For CC BY 4.0, attribution must include: creator, license, license link, and indication of whether
modifications were made.

The SDK makes this mechanically satisfiable:

```swift
// Everything a consumer needs, as ready-to-render strings
for notice in Terra.attribution {
    print(notice)   // "Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0."
}
```

```kotlin
Terra.attribution.forEach { println(it) }
```

A conforming application displays at least:

```swift
Text(Terra.attributionString)   // rendered in an About or Settings screen
```

**TerraKit's obligation is to make attribution easy and impossible to miss. Displaying it is the
application's obligation**, and the docs say so plainly rather than implying the SDK handles it.

Modifications: a package generated by the TerraKit pipeline is a derivative work of GeoNames. Where
the pipeline normalizes or dedupes, the manifest records `modifications` so the notice can state
"modified: normalized and deduplicated".

---

## 6. Pipeline enforcement

Licensing is enforced mechanically, not by review.

| Rule | Enforcement | Failure |
|---|---|---|
| every source has a resolved license | build-time check | **build fails** |
| every source has non-empty attribution | build-time check | **build fails** |
| `licenseUrl` is present and valid | build-time check | **build fails** |
| `retrievedAt` and `checksum` recorded per source | build-time check | **build fails** |
| `entityShare` totals match package entity counts | validation | validation error |
| `manifest.attribution` non-empty | validation | validation error |
| `modifications` declared where applicable | validation | validation error |

This is stricter than §28's "every dataset must contain" and is intentional: a build that fails is
recoverable, a shipped package with hidden attribution is not.

**This is the one rule in the project with no override flag.** There is no
`--ignore-license` and no `--no-attribution`.

---

## 7. Contributor licensing

Contributors keep copyright in their contributions and license them to the project under the same
terms as the existing code (MIT). Contributions of data files must come from a source whose license
permits redistribution under those terms, and must include the provenance metadata in §6.

By contributing, you confirm you have the right to license the contribution and that it does not
introduce a conflicting obligation.

---

## 8. Open questions

1. **Whether to dual-license the code.** MIT is permissive and matches the specification's
   open-source intent. Adding Apache-2.0 would help some corporate adopters. Decision before first
   release.
2. **Trademark.** "TerraKit" as a name is unregistered. Decide whether to file it, and whether to
   require attribution for use of the name in derived products.
3. **Enterprise datasets** (§47). These will carry their own terms, likely not CC BY. The per-source
   license model already supports them; the question is whether they belong in this repository or a
   separate one.
