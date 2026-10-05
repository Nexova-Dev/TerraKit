# ADR-002: Identity derived from external authorities, not from names

Status: **Accepted**
Date: 2026-10-05
Affects: `DATA_MODEL.md` §3, `PACKAGE_FORMAT.md` §8, all SDKs, delta updates

## Context

§6 of the specification requires stable IDs that survive dataset updates, and gives the example
`city_tr_antalya`. It also requires IDs to be deterministic or externally stable "where possible".

The example format does not satisfy the requirement it illustrates. It fails on four cases that
occur in real data.

## Decision

IDs follow `{namespace}:{authority}:{value}`, resolved in strict priority order:

1. ISO 3166-1 alpha-2 for countries (`country:TR`).
2. ISO 3166-2 for administrative areas (`admin:TR-07`, `admin:US-CA`, `admin:JP-27`) — these codes
   exist precisely to be stable across renames.
3. GeoNames IDs for settlements and subdivisions below level 2 (`place:geonames:3054643`).
4. OSM IDs for streets when OSM enters the pipeline (`street:osm:way:1234567`).
5. Deterministic hash over `country + level + type + normalized admin path + name` as a documented
   fallback, counted and rate-limited in validation.

## Why the example fails

**Rename.** `İstanbul` → `Istanbul` → `Constantinople` → `İstanbul`. A name-derived ID changes on
every one of these transitions, orphaning every stored user address that referenced it. §6 requires
the opposite.

**Collision.** GeoNames contains 47 distinct populated places named `Antalya` across Türkiye's
provinces. A name-based ID cannot distinguish them without a disambiguator, and any disambiguator
built from parent names is itself subject to renames.

**Transliteration drift.** `Atatürk` / `Atatürk` / `Ataturk` are one street. Name-derived IDs
fragment into three entities, which is exactly the failure §8 of the specification asks search to
avoid.

**Administrative reorganization.** Türkiye renumbered provinces in 1995, 2004 and 2012. A
name-derived ID silently reassigns stored data to the wrong province — a data corruption bug that
validation cannot detect, because the resulting data is internally consistent.

## Alternatives considered

**UUIDs assigned at ingest.** Rejected. Stable only within one ingest run. A re-run of the pipeline
assigns different UUIDs, so §27's reproducibility requirement is violated outright.

**Natural keys (name + parent + type).** Rejected for the reasons above.

**Name + ISO code hybrid.** Rejected. The name part reintroduces rename fragility while adding
nothing, since the code alone is already stable.

## Consequences

**Positive**
- IDs survive renames, transliteration changes and administrative reorganization.
- Deltas reference stable IDs, so they apply cleanly across unrelated dataset changes
  (see [`ADR-006`](ADR-006-row-level-deltas.md)).
- Re-sync is idempotent: the pipeline looks up `source_row_id` rather than re-deriving identity.

**Negative**
- IDs are meaningless to read. `place:geonames:3054643` tells a human nothing, which hurts debugging
  and log output. Mitigated by a `--explain` mode in the CLI and by `place_names` for display.
- The namespace grammar must be enforced. An unknown namespace is a validation error, never a
  silent pass-through.
- ID vocabulary changes when a new source enters. `admin:` codes get revised by ISO between editions;
  the pipeline must diff ISO revisions and report renames rather than silently re-key.
- The hash fallback is genuinely weaker and is deliberately counted: above 0.5% of places it is a CI
  failure for MVP countries, because it means the source data is too weak to support stable identity.

**Neutral**
- IDs are case-sensitive and never normalized. Lowercasing an ID is a bug.
- SDK consumers must treat IDs as opaque. Parsing them is a documented anti-pattern, because
  authority values change across source revisions.
