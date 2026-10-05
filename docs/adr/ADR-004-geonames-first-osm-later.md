# ADR-004: GeoNames CC-BY first, OSM ODbL later

Status: **Accepted**
Date: 2026-10-05
Affects: source selection, `PACKAGE_FORMAT.md` §3, SDK public API, MVP scope

## Context

§28 permits only legally usable data and names OpenStreetMap and GeoNames as candidates. §29 makes
licensing a first-class feature and states the project "must never hide source attribution".

The two candidate sources have materially different license terms.

| | GeoNames | OpenStreetMap |
|---|---|---|
| License | CC BY 4.0 | ODbL 1.0 |
| Redistribution | permitted | permitted |
| Modification | permitted | permitted |
| Commercial use | permitted | permitted |
| Share-alike | **none** | **required on the derived database** |
| Attribution | required | required |

## Decision

GeoNames is the primary source for the MVP. OSM is planned for streets and neighborhoods only, at a
later stage, with licensing consequences handled explicitly at that point.

Consequences adopted now:

1. **License is declared per package and per source**, never globally. `manifest.license` and
   `manifest.sources[].license` are both present.
2. **`manifest.sources[].entityShare`** records which entities came from which source, so a consumer
   under CC-BY knows precisely what requires attribution.
3. **The pipeline refuses to emit a package whose source has no resolved license.** It fails the
   build. This is stronger than §28's "must contain" and is the mechanism that makes it enforceable
   rather than aspirational.
4. **SDKs expose `Terra.attribution`.** An application must be able to satisfy its own obligations
   without reading our documentation.
5. **Streets are out of MVP scope.** ODbL share-alike is what makes street data legally distinct, and
   mixing license regimes inside one package format is a design mistake to avoid now rather than
   unwind later.

## Alternatives considered

**OSM from day one.** Rejected for MVP. Every package containing OSM-derived data becomes ODbL, which
constrains downstream commercial redistribution and obliges every consuming application to display
attribution. That is a real cost to impose before there is a user base to consult.

**GeoNames only, permanently.** Rejected. OSM remains the right source for streets and for
neighborhood-level detail in many countries. `city:geonames` coverage is thin below city level.

**Vendor data (Google, HERE, TomTom).** Rejected. §28 forbids copying proprietary geographic
databases without permission, and none are licensed for redistribution.

## Consequences

**Positive**
- MVP packages are permissively licensed and freely redistributable.
- No share-alike obligation attaches to the packages or to consumers' derived data.
- Source metadata is a first-class field, so adding OSM later is a data change.

**Negative**
- Less detail than OSM at street and neighborhood level. Accepted for MVP, and the reason streets
  are deferred.
- GeoNames quality varies by country. Coverage per country must be tracked and surfaced
  (`DATA_MODEL.md` §3.3 rate-limits hash-derived IDs for exactly this reason).
- A later OSM-based street package will be ODbL while other packages are CC-BY. Users must handle
  two license regimes. This is disclosed per package rather than hidden.

**Neutral**
- License is data, not code. Adding a source does not change the format, the SDK API or the
  verification flow.
