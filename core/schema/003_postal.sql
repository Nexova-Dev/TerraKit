-- TerraKit postal schema v1
-- core/schema/003_postal.sql
--
-- Normative: DATA_MODEL.md section 8.
-- SPEC section 18 requires both directions and explicitly forbids assuming a
-- one-to-one relationship, so the code and its areas are separate tables.

CREATE TABLE postal_codes (
    id           TEXT PRIMARY KEY,        -- 'postal:TR:07160'
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    code         TEXT NOT NULL,           -- as written: '07160', 'SW1A 1AA'
    code_norm    TEXT NOT NULL,           -- postal_normalize(): alnum only, no space collapse
    place_id     TEXT REFERENCES places(id),   -- most representative place
    latitude     REAL CHECK (latitude  IS NULL OR latitude  BETWEEN  -90 AND  90),
    longitude    REAL CHECK (longitude IS NULL OR longitude BETWEEN -180 AND 180),
    UNIQUE (country_iso2, code)
) WITHOUT ROWID;

CREATE INDEX idx_postal_norm ON postal_codes(code_norm);
CREATE INDEX idx_postal_place ON postal_codes(place_id);

-- One code covers many areas. This table is the reason a naive one-to-one model
-- fails on the US (41k ZIPs, some covering thousands of streets), Germany
-- (four- and five-digit codes nesting) and any country where a code spans
-- several districts.
CREATE TABLE postal_areas (
    postal_code_id TEXT NOT NULL REFERENCES postal_codes(id),
    place_id       TEXT NOT NULL REFERENCES places(id),
    is_primary     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (postal_code_id, place_id)
) WITHOUT ROWID;

CREATE INDEX idx_postal_areas_place ON postal_areas(place_id);

-- code -> location
--   SELECT ... FROM postal_areas a JOIN places p ON p.id = a.place_id
--   WHERE a.postal_code_id = (SELECT id FROM postal_codes WHERE code_norm = ? AND country_iso2 = ?)
--
-- location -> codes
--   SELECT ... FROM postal_areas a JOIN postal_codes c ON c.id = a.postal_code_id
--   WHERE a.place_id = ?
--
-- Both directions are single indexed lookups. See DATA_MODEL.md section 8.
--
-- Countries with no postal system (IE, JP) have no rows. Queries return nil.
-- Absent, not empty-and-broken.
--
-- Postal validation is country-specific and lives in a pipeline-generated
-- table, not in SDK code. ^[0-9]{5}$ is wrong for GB, DE, NL and dozens more.
