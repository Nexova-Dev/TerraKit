# ADR-005: PostGIS in the pipeline, SQLite R*Tree at runtime

Status: **Accepted**
Date: 2026-10-05
Affects: `database/`, `data-pipeline/`, SDKs

## Context

§43 recommends PostgreSQL. §19 requires nearest-location queries across countries, cities, districts,
neighborhoods, streets and postal areas, and states "use spatial indexes where appropriate". §40
requires coordinate lookup and nearest-location benchmarks.

PostgreSQL without PostGIS has no geometry type, no spatial index and no spatial query functions.
There is no implementation of §19 on plain PostgreSQL.

## Decision

**Build time:** PostGIS. `places` carries `geometry(Point, 4326)` with an SpGist index;
`ST_DWithin` and `ST_Distance` serve nearest-location work; bounding boxes are computed for coarse
prefiltering.

**Runtime:** SQLite **R\*Tree** virtual table over populated places, refined with a haversine
distance calculation in the SDK.

```sql
CREATE VIRTUAL TABLE places_rtree USING rtree(id_rowid, min_lat, max_lat, min_lon, max_lon);
```

## Why R\*Tree rather than a bounding-box prefilter

The obvious implementation of "find nearest" is a bounding-box filter followed by haversine sorting.
It is the wrong choice and it degrades exactly where data is densest.

At 60° N (Antalya is 36.9°, but Oslo, Helsinki and Stockholm are the dense cases) a 1 km radius
produces a bounding box roughly 1 km wide and 2 km tall. The box contains almost every point in it —
it filters nothing, and the scan runs over the full candidate set. The further north, the worse the
box's selectivity, because a fixed metric distance spans a shrinking range of longitude.

At the equator the same 1 km box is close to square and filters well. So the prefilter performs well
in some regions and degenerates in others, which is the worst failure profile for a spatial index:
users discover it as "nearest location is slow in Scandinavia", not as a uniform slowdown.

R\*Tree indexes bounding boxes hierarchically and traverses them in order of proximity, so the cost
is proportional to the number of results, not to the density of the region. It is the same reason
PostGIS defaults to GiST rather than a bbox-and-refine strategy.

**R\*Tree is a candidate generator, not the answer.** It returns candidate rowids in box-distance
order; the SDK computes haversine over the candidates and ranks them correctly. Exact ordering always
happens in application code.

## Alternatives considered

**Bounding-box prefilter plus haversine.** Rejected for the latitude-dependent selectivity problem
above.

**Brute-force haversine over all places.** Rejected. `O(n)` per query at millions of records violates
§40 and §41.

**PostGIS in the SDK.** Rejected. PostGIS is a server-side extension; it would make the SDK a network
client and destroy §15's offline guarantee.

**H3 or S2 cells in SQLite.** Deferred. Attractive for clustering and tiling, but it adds a
dependency and a second indexing scheme for no gain at MVP record counts. Worth revisiting when
boundaries arrive (§46).

## Consequences

**Positive**
- Both sides use a spatial index, so §19 is satisfiable at both ends of the pipeline.
- The SDK needs no geospatial dependency: R\*Tree ships with SQLite, and haversine is four lines.
- Runtime remains fully offline.

**Negative**
- The pipeline depends on PostGIS, so `docker compose up` is effectively mandatory for development.
  This is a heavier local setup than plain PostgreSQL, accepted because §19 requires it.
- R\*Tree stores bounding boxes, so a place with an imprecise centroid yields a box that is wrong by
  that imprecision. Acceptable for populated places; administrative areas use bounding boxes directly
  (`DATA_MODEL.md` §7).
- R\*Tree rowid must map to `places.id`. An integer surrogate column is required on `places`. It is
  internal, never exposed in the SDK API — public identity remains the namespaced string ID
  ([`ADR-002`](ADR-002-identity-from-external-authorities.md)).
