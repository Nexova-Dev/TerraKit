# ADR-001: Unified `places` table instead of per-entity-type tables

Status: **Accepted**
Date: 2026-10-05
Affects: `DATA_MODEL.md`, pipeline, all SDKs

## Context

The source specification (§13) prescribes separate tables: `regions`, `states`, `provinces`,
`counties`, `districts`, `neighborhoods`. It also states (§4, §48, §56.7) that TerraKit must not
assume every country has the same hierarchy, and deliberately includes Japan in the test set (§52)
because its structure differs substantially from Western models.

These two requirements are in direct conflict.

## Decision

A single `places` table with `type` (semantic) and `level` (structural depth) columns, plus a
`place_closure` table for precomputed ancestry. Postal codes, continents, currencies, timezones and
languages get dedicated tables because their cardinality and query patterns differ.

## Alternatives considered

**Per-type tables as specified.** Rejected. Countries have different level counts, so each table
accumulates nullable columns and empty tables; every cross-level query becomes a `UNION`; adding a
new area type requires a migration. Japan has no county layer at all — the `counties` table would be
empty for an entire MVP country.

**Fully normalized EAV (entity-attribute-value).** Rejected. Maximum flexibility, but every filter
is a two-table join and the performance targets in §40 (<10 ms lookup) become hard to reach.

**Document store, one JSON blob per place.** Rejected. Same reason as §13 rejects a single giant
JSON document: no indexing, no type safety, full parse per row.

**Hybrid: `places` plus per-type extension tables.** Deferred. Worth adding for streets, where
attributes are dense and street-specific (`surface`, `oneway`, `length_m`). Not needed for MVP,
where all attributes fit the unified table.

## Consequences

**Positive**
- Any hierarchy depth from 1 to 7 works with no schema change.
- Descendant queries are one indexed join against `place_closure`.
- Hierarchy traversal in each SDK is one function, not seven.
- Adding a new area type is a data change, not a migration.

**Negative**
- No database-level type safety between a parent and its child. Mitigated by a level transition
  constraint enforced in the pipeline validator.
- `places` is the largest table in every package; per-country query locality depends on the
  `(country_iso2, level, ...)` indices.
- Ad-hoc SQL against `places` is less self-documenting than a table named `districts`. The SDK
  exists precisely so users never write that SQL.

**Neutral**
- `type` and `level` are now distinct concepts that every consumer must understand. This is a
  feature: it is the distinction that keeps §1 of `DATA_MODEL.md` honest.
