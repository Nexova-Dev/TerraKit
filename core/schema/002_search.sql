-- TerraKit search schema v1
-- core/schema/002_search.sql
--
-- Normative: DATA_MODEL.md section 9.
-- Read that section and docs/adr/ADR-003-search-mechanisms.md before changing anything.
--
-- Requires SQLite >= 3.34 for the FTS5 trigram tokenizer.

-- ---------------------------------------------------------------------------
-- fuzzy / substring / infix search
-- ---------------------------------------------------------------------------
CREATE VIRTUAL TABLE search_fts USING fts5(
    name_norm,
    name_norm_unaccented,
    place_id UNINDEXED,
    tokenize = 'trigram'
);

-- Trigram needs 3+ characters to produce a token. Queries shorter than that
-- must be routed to idx_places_norm in the SDK. This is documented API
-- behaviour, not a hidden fallback: see DATA_MODEL.md section 9.

-- ---------------------------------------------------------------------------
-- phonetic codes — precomputed by the pipeline
-- ---------------------------------------------------------------------------
-- SDKs NEVER compute these. Implementing Double Metaphone in Swift, Kotlin and
-- Rust and keeping all three byte-identical is a liability with no benefit:
-- agreeing becomes a data property CI verifies once (ADR-003).
CREATE TABLE phonetic_codes (
    code     TEXT NOT NULL,              -- Double Metaphone
    place_id TEXT NOT NULL REFERENCES places(id),
    PRIMARY KEY (code, place_id)
) WITHOUT ROWID;

-- ---------------------------------------------------------------------------
-- spatial index — DATA_MODEL.md section 7, ADR-005
-- ---------------------------------------------------------------------------
-- R*Tree, not a bounding-box prefilter plus LIKE. At high latitudes a 1 km box
-- filters almost nothing, so a prefilter degrades exactly where data is
-- densest. R*Tree cost is proportional to result count, not regional density.
--
-- id_rowid maps to places.rowid_int, an internal surrogate. Public identity
-- remains the namespaced string ID and is never exposed from the SDK API.
CREATE VIRTUAL TABLE places_rtree USING rtree(
    id_rowid,
    min_lat, max_lat,
    min_lon, max_lon
);

-- R*Tree is a candidate generator, never the answer. The SDK computes haversine
-- over candidates and ranks them. Exact ordering always happens in application
-- code.
