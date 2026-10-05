-- TerraKit core schema v1
-- core/schema/001_core.sql
--
-- Normative: DATA_MODEL.md sections 2, 4, 6, 7, 10.
-- Read DATA_MODEL.md before changing anything here. Most of it is explained there.
--
-- Requires SQLite >= 3.34 (FTS5 trigram tokenizer, ADR-003).

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- metadata — read once at open
-- ---------------------------------------------------------------------------
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
-- Keys: schema_version, schema_min_reader_version, generator_version,
--       dataset_version, source_date_epoch, place_count, package_sha256
-- Timestamps come from SOURCE_DATE_EPOCH, never the wall clock (ARCHITECTURE.md 11).

-- ---------------------------------------------------------------------------
-- reference data
-- ---------------------------------------------------------------------------
CREATE TABLE continents (
    code      TEXT PRIMARY KEY,          -- 'EU', 'AS'
    name      TEXT NOT NULL,
    name_norm TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE languages (
    code      TEXT PRIMARY KEY,          -- 'tr' ISO 639-1
    iso639_3  TEXT NOT NULL,
    name      TEXT NOT NULL,
    name_norm TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE currencies (
    code        TEXT PRIMARY KEY,        -- 'TRY' ISO 4217
    name        TEXT NOT NULL,
    symbol      TEXT,
    minor_units INTEGER NOT NULL
) WITHOUT ROWID;
-- No exchange rates. SPEC section 22 excludes them from core.

CREATE TABLE timezones (
    id               TEXT PRIMARY KEY,   -- 'Europe/Istanbul', IANA
    name             TEXT NOT NULL,
    utc_offset_min   INTEGER NOT NULL    -- ADVISORY only, see below
) WITHOUT ROWID;
-- utc_offset_min is for display/sorting convenience and does NOT account for DST.
-- Never use it as the current time. SDKs return a TimeZone/ZoneId, not an offset.

-- ---------------------------------------------------------------------------
-- countries — DATA_MODEL.md section 6
-- ---------------------------------------------------------------------------
CREATE TABLE countries (
    id               TEXT PRIMARY KEY,               -- 'country:TR'
    iso2             TEXT NOT NULL UNIQUE,
    iso3             TEXT NOT NULL UNIQUE,
    numeric_code     TEXT NOT NULL,
    name             TEXT NOT NULL,
    name_norm        TEXT NOT NULL,
    continent_code   TEXT NOT NULL REFERENCES continents(code),
    capital_place_id TEXT REFERENCES places(id),
    currency_code    TEXT NOT NULL REFERENCES currencies(code),
    phone_code       TEXT,                           -- '+90'
    internet_tld     TEXT,                           -- '.tr'
    driving_side     TEXT CHECK (driving_side IN ('left', 'right')),
    latitude         REAL CHECK (latitude  IS NULL OR latitude  BETWEEN  -90 AND  90),
    longitude        REAL CHECK (longitude IS NULL OR longitude BETWEEN -180 AND 180),
    min_latitude     REAL,
    max_latitude     REAL,
    min_longitude    REAL,
    max_longitude    REAL,
    flag_emoji       TEXT,
    independent      INTEGER NOT NULL DEFAULT 1
) WITHOUT ROWID;

CREATE INDEX idx_countries_norm ON countries(name_norm);

CREATE TABLE country_languages (
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    lang         TEXT NOT NULL REFERENCES languages(code),
    is_official  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (country_iso2, lang)
) WITHOUT ROWID;

CREATE TABLE country_currencies (
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    currency_code TEXT NOT NULL REFERENCES currencies(code),
    is_primary   INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (country_iso2, currency_code)
) WITHOUT ROWID;

CREATE TABLE country_timezones (
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    timezone_id  TEXT NOT NULL REFERENCES timezones(id),
    PRIMARY KEY (country_iso2, timezone_id)
) WITHOUT ROWID;

-- ---------------------------------------------------------------------------
-- places — DATA_MODEL.md sections 1, 2, 3, 7
-- One table for every hierarchical entity, at any depth, in any country.
-- See docs/adr/ADR-001-unified-places-table.md.
--
-- type  = semantic: 'city', 'town', 'district', 'neighborhood', ...
-- level = structural depth: 2..7, steps below the country.
-- They are NOT the same thing; see DATA_MODEL.md section 1.
-- ---------------------------------------------------------------------------
CREATE TABLE places (
    id             TEXT PRIMARY KEY,      -- 'place:geonames:3054643'
    rowid_int      INTEGER UNIQUE,        -- internal, for R*Tree only; never exposed in SDK APIs
    type           TEXT NOT NULL,
    -- Level 1 is the country row itself (the hierarchy anchor), level 2 its top
    -- administrative area, up to 7. SPEC section 4 puts Country inside the
    -- hierarchy, so a settlement with a broken upstream admin1 reference can
    -- still attach to something real instead of becoming an orphan.
    level          INTEGER NOT NULL CHECK (level BETWEEN 1 AND 7),
    -- The country row satisfies this by being the only level-1 place.
    country_iso2   TEXT NOT NULL REFERENCES countries(iso2),
    parent_id      TEXT REFERENCES places(id),
    name           TEXT NOT NULL,
    name_norm      TEXT NOT NULL,
    admin_code     TEXT,                  -- ISO 3166-2 where applicable
    identity_source TEXT NOT NULL CHECK (identity_source IN ('iso3166-2', 'geonames', 'osm', 'hash')),
    latitude       REAL CHECK (latitude  IS NULL OR latitude  BETWEEN  -90 AND  90),
    longitude      REAL CHECK (longitude IS NULL OR longitude BETWEEN -180 AND 180),
    population     INTEGER,
    timezone_id    TEXT REFERENCES timezones(id),
    currency_code  TEXT REFERENCES currencies(code),
    search_rank    INTEGER NOT NULL DEFAULT 0,
    source_id      TEXT NOT NULL REFERENCES data_sources(id),
    source_row_id  TEXT,
    updated_at     TEXT NOT NULL
) WITHOUT ROWID;

-- Exact and prefix search. The composite form serves prefix queries too.
CREATE INDEX idx_places_norm      ON places(name_norm);
CREATE INDEX idx_places_norm_type ON places(name_norm, type);
-- parent -> children, the most common traversal
CREATE INDEX idx_places_parent    ON places(parent_id, type, name_norm);
-- structural queries
CREATE INDEX idx_places_level     ON places(country_iso2, level, name_norm);
-- semantic queries with rank ordering
CREATE INDEX idx_places_type      ON places(country_iso2, type, search_rank DESC, name_norm);
-- administrative code lookups
CREATE INDEX idx_places_admin     ON places(country_iso2, admin_code);

-- ---------------------------------------------------------------------------
-- closure table — DATA_MODEL.md section 4
-- Every package ships it fully materialized. SDKs run zero recursive queries.
-- ---------------------------------------------------------------------------
CREATE TABLE place_closure (
    ancestor_id   TEXT NOT NULL REFERENCES places(id),
    descendant_id TEXT NOT NULL REFERENCES places(id),
    depth         INTEGER NOT NULL CHECK (depth BETWEEN 0 AND 7),
    PRIMARY KEY (ancestor_id, descendant_id, depth)
) WITHOUT ROWID;

CREATE INDEX idx_closure_descendant ON place_closure(descendant_id, depth);

-- ---------------------------------------------------------------------------
-- names and localization — DATA_MODEL.md section 5
-- ---------------------------------------------------------------------------
CREATE TABLE place_names (
    place_id    TEXT NOT NULL REFERENCES places(id),
    lang        TEXT NOT NULL,           -- BCP-47
    script      TEXT,                    -- ISO 15924
    name        TEXT NOT NULL,
    name_norm   TEXT NOT NULL,
    is_preferred INTEGER NOT NULL DEFAULT 0,
    source      TEXT NOT NULL,
    PRIMARY KEY (place_id, lang)
) WITHOUT ROWID;

CREATE INDEX idx_place_names_norm ON place_names(name_norm, lang);

CREATE TABLE place_alternate_names (
    place_id  TEXT NOT NULL REFERENCES places(id),
    name      TEXT NOT NULL,
    name_norm TEXT NOT NULL,
    lang      TEXT,
    class     TEXT,     -- 'exonym'|'colloquial'|'transliteration'|'historical'|'abbreviation'
    source    TEXT NOT NULL,
    PRIMARY KEY (place_id, name, lang, class)
) WITHOUT ROWID;

CREATE INDEX idx_alt_norm ON place_alternate_names(name_norm);

-- ---------------------------------------------------------------------------
-- provenance — DATA_MODEL.md section 11, LICENSING.md section 6
-- ---------------------------------------------------------------------------
CREATE TABLE data_sources (
    id           TEXT PRIMARY KEY,        -- 'geonames'
    name         TEXT NOT NULL,
    version      TEXT NOT NULL,
    license      TEXT NOT NULL,
    license_url  TEXT NOT NULL,
    attribution  TEXT NOT NULL,
    url          TEXT NOT NULL,
    retrieved_at TEXT NOT NULL,
    checksum     TEXT
) WITHOUT ROWID;
-- The pipeline refuses to emit a package without a resolved license per source.
-- There is no override flag for this. See LICENSING.md section 6.
