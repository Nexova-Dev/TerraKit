# ADR-006: Row-level deltas rather than file-level or block-level

Status: **Accepted** (design now, implementation after MVP)
Date: 2026-10-05
Affects: `PACKAGE_FORMAT.md` §8, `DATA_MODEL.md` §3, SDKs

## Context

§33 sets a target: Türkiye v2026.10.0 → v2026.10.1, where a full package is 100 MB and the delta is
2.5 MB. §32 requires updates to be optional, versioned, verifiable, resumable and backward
compatible. §33 states deltas come after the MVP.

The scheme is deferred, but the identity system it depends on is not deferrable, and choosing it now
is what makes the delta design possible later.

## Decision

**Row-level operation log.** The pipeline emits `delta.sqlite` containing explicit `insert`,
`update` and `delete` operations keyed by stable IDs from
[`ADR-002`](ADR-002-identity-from-external-authorities.md), plus a `delta_meta` table naming the base
and target versions.

```sql
CREATE TABLE delta_ops (
    seq         INTEGER PRIMARY KEY,
    op          TEXT NOT NULL CHECK (op IN ('insert','update','delete')),
    table_name  TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    payload     TEXT
);
```

The SDK applies operations in a single transaction, after which the package is at the target version.

## Alternatives considered

**Block-level binary diff.** Rejected. Requires an encoding inside the SQLite file that understands
its page and cell structure, so a delta applies to exactly one exact source file and no other. It
also produces large deltas when a single row change dirties many pages — the common case for a
bulk-updated dataset. Corruption cascades through a whole block.

**Whole-file shard replace.** Accepted as a fallback, not as the primary scheme. With enough shards
the delta approximates the block-level ideal, but the record counts force sharding thresholds into
MVP territory, where per-shard FTS5 and ranking have to be merged in application code and the result
ordering stops being deterministic.

**Rebuild locally.** Rejected. Requires the full dataset and a build toolchain on the user's machine,
which defeats the purpose of a package manager and breaks §15's offline promise in spirit.

## Consequences

**Positive**
- Deltas survive unrelated changes. A delta referencing `place:geonames:3054643` applies cleanly even
  if 50 000 other rows changed between base and target. Block-level diffs cannot do this.
- Deltas apply to a package several versions behind, not only to the exact base version.
- `delta.sqlite` is inspectable with any SQLite client, so a failed delta is diagnosable.
- Failure is atomic: a transaction rollback leaves the existing package untouched.

**Negative**
- Package size is not the optimization target; application time is. A delta that is small but slow to
  apply is a regression. The acceptance criterion is both size (<10% of full) and time (faster than
  a re-download), and a delta failing the time test ships as a full package.
- Deletes and cascading updates across `places`, `place_names`, `place_alternate_names`,
  `place_closure`, `postal_areas` and `search_fts` must all be emitted correctly. This is the real
  cost of the scheme, and it is why implementation is deferred rather than attempted now.
- `place_closure` rebuilds can cascade widely. A hierarchy change in one province rewrites closure
  rows for a large subtree.

**Neutral**
- `terrakit update` keeps a backup of `data.sqlite` before applying, making rollback a file copy.
- The delta format needs no `packageFormatVersion` bump relative to the base format, since it is a
  distinct artifact type declared in the manifest.
