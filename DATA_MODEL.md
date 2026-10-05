# TerraKit Data Model

Status: **Draft / Accepted for MVP**
Last updated: 2026-10-05
Normative: where this document and code disagree, this document is correct and the code is a bug.

This document defines the entity model, the identity system, the Unicode normalization algorithm
and the SQLite schema that ships inside every package. See [`ARCHITECTURE.md`](ARCHITECTURE.md) for
why these choices were made and [`PACKAGE_FORMAT.md`](PACKAGE_FORMAT.md) for how they are packaged.

---

## 1. Core principle: `type` and `level` are different things

The single most consequential modelling decision.

**`level` is structural depth.** It is an integer from 1 to 7 and means "how many steps from the
country". Level 3 is whatever sits three steps below the country, whether that is a French
department, a Japanese district or a Turkish district.

**`type` is semantic meaning.** It answers "what kind of place is this" — `city`, `town`, `village`,
`district`, `neighborhood`, `municipality`, `borough`, `ward`.

Conflating them is how existing address libraries break. Turkey has a `district` at level 3. The
United Kingdom has a `borough` at level 3 and its `district`/`ward` at level 4. Japan has a
`district` at level 4 with no county anywhere. A library that hardcodes "district means level 3"
produces wrong addresses in two of three countries.

Queries therefore filter on `level` for structure and on `type` for meaning:

```swift
// structural: everything two steps below the country
turkey.places(atLevel: 3)

// semantic: settlements, regardless of depth
turkey.places(ofType: .city)
```

`level` is bounded at 7 to keep closure-table traversal cheap and to match the deepest hierarchy in
§4 of the specification. The bound is a performance decision, not a claim about the world;
hierarchies deeper than 7 collapse into the deepest level with a `subdivision_count` hint.

---

## 2. Entity inventory

The specification lists 19 entities in §5. They fall into three groups by storage strategy.

**Stored in the unified `places` table** — anything that participates in the administrative
hierarchy, at any depth, in any country:

| type | notes |
|---|---|
| `country` | not in `places`; has its own table for ISO metadata, see §6 |
| `region` | generic admin area, level 2–3 |
| `state` | US, AU, IN, BR |
| `province` | TR, CA, ZA, ES |
| `county` | US, GB, TR (ilçe) |
| `administrative_area` | fallback when the source will not commit to a type |
| `city` | populated place, any population |
| `town` | `population_class` distinguishes from `city` where the source provides it |
| `village` | |
| `district` | §1 — level varies by country |
| `neighborhood` | TR mahalle, JP chōme, DE Stadtteil |
| `municipality` | DE Gemeinde, NL gemeente |
| `borough` | GB, JP, NYC |
| `ward` | JP, IN, GB |
| `suburb` | AU, ZA |
| `prefecture` | JP |
| `state_district` | TR il |
| `canton` | CH |
| `department` | FR |
| `parish` | many |

**Stored in dedicated tables** — entities with their own cardinality or query pattern:

| Entity | Why separate |
|---|---|
| `postal_codes` | many-to-many with places; §18 explicitly forbids one-to-one |
| `continents` | small, static, joined often |
| `timezones` | IANA identifiers, referenced by name |
| `languages` | ISO 639-1/639-3 |
| `currencies` | ISO 4217 |
| `flags` | SVG/asset metadata, not core data |

**Derived, never stored as truth:** coordinates (stored, but see §7), population (advisory),
boundaries (out of MVP, §46).

---

## 3. Identity

### 3.1 Grammar

```text
identifier := namespace ":" authority ":" authority_value
```

Namespaces are closed and exhaustive. An unknown namespace is a validation error, never a silent
pass-through — an unparseable ID in a package is a corrupt package.

| namespace | authority | example | scope |
|---|---|---|---|
| `country` | ISO 3166-1 alpha-2 | `country:TR` | countries |
| `admin` | ISO 3166-2 | `admin:TR-07`, `admin:US-CA`, `admin:JP-27` | level 2–4 admin areas |
| `place` | `geonames` | `place:geonames:3054643` | settlements, subdivisions below admin |
| `street` | `osm` | `street:osm:way:1234567` | streets, when OSM enters |
| `postal` | ISO 3166-1 + code | `postal:TR:07160` | postal codes |
| `hash` | — | `hash:sha256:9f2a...` | fallback, see §3.3 |

Two consequences that are part of the contract:

- **IDs are opaque to SDK users.** They are stable, comparable and sortable for equality, but
  consumers must not parse them. Parsing is a documented anti-pattern in
  [`SDK_SPEC.md`](SDK_SPEC.md) because authority values change: ISO 3166-2 codes get revised, and
  relying on a parse means shipping a breaking change to every consumer.
- **IDs are case-sensitive and never normalized.** Lowercasing them is a bug.

### 3.2 Why not the specification's example

§6 gives `city_tr_antalya`. It fails on four cases that all occur in real data:

1. **Rename.** `city:geonames:3054643` stays valid through Istanbul → İstanbul → Constantinople.
   `city_tr_istanbul` becomes a dangling reference for every stored user address.
2. **Collision.** GeoNames has 47 distinct populated places named `Antalya` across TR's provinces.
   A name-based ID cannot distinguish them without a disambiguator, and the disambiguator is
   itself unstable.
3. **Transliteration change.** `Atatürk` → `Atatürk` → `Ataturk` are the same street. Name-based
   IDs fragment.
4. **Administrative reorganization.** TR renumbered provinces repeatedly (1995, 2004, 2012). A
   name-derived ID silently reassigns data to the wrong province.

GeoNames IDs are stable across all of these. That is the entire reason to adopt them.

### 3.3 Fallback hierarchy

When building a package the pipeline resolves identity in strict priority order and records which
rule fired in `places.identity_source`:

1. ISO 3166-2 code available → `admin:<code>` (`identity_source = 'iso3166-2'`)
2. GeoNames ID available → `place:geonames:<id>` (`identity_source = 'geonames'`)
3. Neither → deterministic hash over `country + level + type + normalized full admin path + name`
   (`identity_source = 'hash'`)

Rule 3 is a last resort and **must be counted**. A package with more than 0.5% hash-derived IDs is
treated as a warning in `terra validate` and as a failure in CI for MVP countries, because it means
the source data is too weak to support stable identity. This is why [`LICENSING.md`](LICENSING.md)
and [`DATA_PIPELINE.md`](DATA_PIPELINE.md) track source coverage per country.

### 3.4 Stability guarantees

| Guarantee | Scope |
|---|---|
| IDs never change between package versions for the same real-world entity | yes, for `iso3166-2` and `geonames` IDs |
| IDs survive upstream renames | yes, for rule 1 and 2 |
| IDs survive upstream ID reassignment | **no** — GeoNames occasionally recycles IDs; the pipeline detects this via coordinate drift and reports it rather than silently reusing the ID |
| IDs are stable across SDK versions | yes |
| IDs are comparable across packages of different countries | yes, namespaces prevent collision |

---

## 4. Hierarchy and the closure table

`places.parent_id` gives direct ancestry. `place_closure` gives all of it, precomputed.

```sql
CREATE TABLE place_closure (
    ancestor_id   TEXT NOT NULL REFERENCES places(id),
    descendant_id TEXT NOT NULL REFERENCES places(id),
    depth         INTEGER NOT NULL,   -- 0 = self
    PRIMARY KEY (ancestor_id, descendant_id, depth)
) WITHOUT ROWID;

CREATE INDEX idx_closure_descendant ON place_closure(descendant_id, depth);
```

`place_closure` is populated at build time by a single set-based insert, not by recursive traversal
per row:

```sql
INSERT INTO place_closure (ancestor_id, descendant_id, depth)
SELECT p.parent_id, c.descendant_id, c.depth + 1
FROM place_closure c
JOIN places p ON p.id = c.ancestor_id
WHERE c.depth < :max_level;
```

Every package ships `closure` fully materialized. A consumer needs **zero** recursive queries, which
removes any possibility of a query-time cycle and bounds worst-case traversal at 7 steps.

Cost: for TR at `cities` level, roughly 6 000 places produce about 24 000 closure rows. Storage is
not the constraint; query speed is, and the `WITHOUT ROWID` primary key makes both lookups a single
B-tree descent.

**Cycle prevention.** `parent_id` must point to a place with strictly smaller `level`, and the
validator rejects any cycle before packaging. Self-parenting is a hard error.

---

## 5. Names, localization and normalization

### 5.1 Storage

Names live in a separate table, not on `places`, because one place has many names in many languages
and one language's name is not more authoritative than another's.

```sql
CREATE TABLE place_names (
    place_id    TEXT NOT NULL REFERENCES places(id),
    lang        TEXT NOT NULL,          -- BCP-47: 'en', 'tr', 'de', 'ka'
    script      TEXT,                   -- ISO 15924: 'Latn', 'Cyrl', 'Geor', 'Jpan'
    name        TEXT NOT NULL,
    name_norm   TEXT NOT NULL,          -- §5.3
    is_preferred INTEGER NOT NULL DEFAULT 0,
    source      TEXT NOT NULL,          -- 'geonames_primary' | 'geonames_alt' | 'osm' | 'manual'
    PRIMARY KEY (place_id, lang)
) WITHOUT ROWID;
```

`places.name` and `places.name_norm` remain as **denormalized defaults** — the English name and its
normalization — because the majority of queries filter on `name_norm` and a join on every row of a
500 000-row table is not acceptable. `place_names` is the authority; the denormalized columns are
maintained by the pipeline and verified equal by the validator.

Fallback chain from §7 of the specification, implemented identically in all SDKs:

```text
requested language → locale fallback (tr-TR → tr → default) → 'en' → places.name (local/original)
```

Missing translations are normal, not an error. §7 says so explicitly, and a package must never fail
validation because a place has only one name.

### 5.2 Transliteration policy

§8 requires `Atatürk`, `Ataturk` and `ATATURK` to resolve to the same entity, and §52 requires
Georgia and Japan in the test set. Those need cross-script handling: Georgian script `თურქეთი` and
Japanese `イスタンブール` must be findable.

**Policy:** the pipeline stores the transliterations supplied by the source (GeoNames provides
alternate names including `Latin` transliterations) as ordinary rows in `place_alternate_names` with
`name_norm` populated. The SDK does **not** implement a transliteration engine at query time.

This is a deliberate trade-off. A query-time transliterator would have to exist in Swift, Kotlin and
Rust and would have to agree across all three — a recurring source of mismatches for a search path
where recall, not exactness, is what matters. Precomputed rows are data, cheap to query with an
index, and verifiable in CI. If measurement on the GE and JP fixtures shows recall is insufficient
([`ARCHITECTURE.md` §13](ARCHITECTURE.md#13-open-questions)), a Cyrillic/Greek table is added to the
pipeline — still not to the SDKs.

### 5.3 Normalization algorithm (normative)

Every implementation — Python, Rust, Swift, Kotlin — must produce byte-identical output. The test
vector set in `tests/fixtures/normalization.json` is the conformance oracle and is executed in CI
for each language.

```text
normalize(input):
  1.  Unicode NFKD decomposition
  2.  Strip combining marks       (Unicode category Mn, Mc, Me)
  3.  Special-case substitutions  (§5.3.1, applied before case folding)
  4.  Full Unicode case folding   (not lowercase(); see §5.3.2)
  5.  Transliterate non-Latin     (§5.3.3, pipeline only)
  6.  Replace all runs of [^\p{L}\p{N}] with a single space
  7.  Trim leading/trailing space
  8.  Collapse internal runs of spaces
```

Step 4 is **case folding**, not lowercasing. This is not pedantry: Turkish `İ` (U+0130) folds to
`i` + combining dot, which step 2 has already removed, yielding `i` — while `I` (U+0049) under a
Turkish locale lowercases to `ı` (U+0131), which is a *different letter*. Locale-sensitive
lowercasing makes `Istanbul` unfindable, and `Istanbul` is the first example in §8 of the
specification.

### 5.3.1 Special-case substitutions

Applied in order after mark stripping, before case folding.

| Input | Output | Reason |
|---|---|---|
| `ß` | `ss` | German; uppercase `ẞ` is a separate code point and also maps to `ss` |
| `æ` `ǽ` | `ae` | |
| `œ` | `oe` | |
| `ø` `Ø` | `o` | Danish/Norwegian |
| `đ` `Đ` | `d` | Croatian, Vietnamese |
| `þ` `Þ` | `th` | Icelandic |
| `ł` `Ł` | `l` | Polish |
| `ı` `I` | `i` | Turkish dotless and dotted I, forced together |
| `ħ` `Ħ` | `h` | Maltese |
| `ŋ` `Ŋ` | `ng` | |
| `ŧ` `Ŧ` | `t` | |
| `ə` | `e` | Azerbaijani |
| `­` | *(removed)* | soft hyphen, U+00AD, an invisible trap |

Soft hyphen is called out separately because it survives NFKD and case folding, matches nothing
in a `LIKE` comparison the user expects, and silently breaks matches in pasted text.

### 5.3.2 Case folding

Full Unicode case folding per `CaseFolding.txt` with `F` and `C` mappings applied, `T` (Turkic)
mappings **not** applied. The `T` mappings are exactly the ones that break cross-language
consistency: applying them in Turkish and not in English makes the same query string produce
different keys.

### 5.3.3 Worked examples

Verified against §8 of the specification:

| Input | Normalized | Matches |
|---|---|---|
| `Istanbul` | `istanbul` | ✓ |
| `İstanbul` | `istanbul` | ✓ |
| `istanbul` | `istanbul` | ✓ |
| `ISTANBUL` | `istanbul` | ✓ |
| `Atatürk` | `ataturk` | ✓ |
| `Ataturk` | `ataturk` | ✓ |
| `ATATURK` | `ataturk` | ✓ |
| `Muratpaşa` | `muratpasa` | ✓ |
| `Türkiye` | `turkiye` | ✓ |
| `Straße` | `strasse` | ✓ |
| `Ağrı` | `agri` | ✓ |
| `İzmir` | `izmir` | ✓ |
| `Tromsø` | `tromso` | ✓ |
| `Łódź` | `lodz` | ✓ |
| `  Antalya  ` | `antalya` | ✓ |
| `San­ta` (soft hyphen) | `santa` | ✓ |
| `İstanbul Cd.` | `istanbul cd` | ✓ punctuation → space |
| `Café` | `cafe` | ✓ |
| `ท⻍าเมือง` | *(unchanged; Thai has no Latin form in this policy)* | exact match only |

### 5.4 Alternate names

```sql
CREATE TABLE place_alternate_names (
    place_id  TEXT NOT NULL REFERENCES places(id),
    name      TEXT NOT NULL,
    name_norm TEXT NOT NULL,
    lang      TEXT,
    class     TEXT,      -- 'exonym' | 'colloquial' | 'transliteration' | 'historical' | 'abbreviation'
    source    TEXT NOT NULL,
    PRIMARY KEY (place_id, name, lang, class)
) WITHOUT ROWID;

CREATE INDEX idx_alt_norm ON place_alternate_names(name_norm);
```

This table is what makes `Terra.search("Constantinople")` return İstanbul, and what makes a Japanese
user find İstanbul by its Japanese name. It is a significant part of the package; §40's memory rule
applies, so it is queried by index and never enumerated.

---

## 6. Country metadata

Countries get a dedicated table rather than living in `places`. §20 requires 13 distinct metadata
fields and they are all static reference data, not hierarchical.

```sql
CREATE TABLE countries (
    id                TEXT PRIMARY KEY,          -- 'country:TR'
    iso2              TEXT NOT NULL UNIQUE,
    iso3              TEXT NOT NULL UNIQUE,
    numeric_code      TEXT NOT NULL,
    name              TEXT NOT NULL,
    name_norm         TEXT NOT NULL,
    continent_code    TEXT NOT NULL REFERENCES continents(code),
    capital_place_id  TEXT REFERENCES places(id),
    currency_code     TEXT NOT NULL REFERENCES currencies(code),
    phone_code        TEXT,                      -- '+90'
    internet_tld      TEXT,                      -- '.tr'
    driving_side      TEXT CHECK (driving_side IN ('left','right')),
    latitude          REAL,
    longitude         REAL,
    min_latitude      REAL, max_latitude REAL,   -- bounding box for R*Tree seeding
    min_longitude     REAL, max_longitude REAL,
    flag_emoji        TEXT,
    independent       INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE country_languages (
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    lang         TEXT NOT NULL,
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
```

`country_timezones` exists because §19 and §21 require nearest-location results to carry a timezone,
and Antalya returns `Europe/Istanbul` while Tokyo returns `Asia/Tokyo`. Storing the IANA identifier
is mandatory — §21 forbids a proprietary timezone system.

---

## 7. Coordinates

```sql
-- on places, nullable: neighborhoods often have no reliable centroid
latitude  REAL CHECK (latitude  IS NULL OR (latitude  BETWEEN  -90 AND  90)),
longitude REAL CHECK (longitude IS NULL OR (longitude BETWEEN -180 AND 180))
```

Nullability is the honest modelling choice. §5 says every entity should support optional
coordinates, and §30 lists invalid coordinates as a validation error — but centroid quality varies
enormously by source and place size. Forcing a value produces a number that looks authoritative and
is not, which is worse for a delivery or banking application than an explicit null.

Populated places always have coordinates. Administrative areas take the area centroid; very large
areas additionally get `min_latitude`/`max_latitude`/`min_longitude`/`max_longitude` bounding boxes
on `countries` for the same reason.

**Nearest-location queries** (§19) use an R\*Tree virtual table built over populated places:

```sql
CREATE VIRTUAL TABLE places_rtree USING rtree(id_rowid, min_lat, max_lat, min_lon, max_lon);
```

R\*Tree, not a bounding-box prefilter plus `LIKE`. See
[`ARCHITECTURE.md` §4](ARCHITECTURE.md#4-runtime-query-strategy) for why the prefilter degrades
precisely where data is densest.

**HaVERSine refinement.** R\*Tree returns candidates ordered by box distance; the final ranking
applies haversine over latitude/longitude. The R\*Tree itself is only a candidate generator, never
the answer.

---

## 8. Postal codes

§18 requires both directions and explicitly forbids assuming a one-to-one relationship. The schema
therefore separates the code from the places it covers.

```sql
CREATE TABLE postal_codes (
    id          TEXT PRIMARY KEY,        -- 'postal:TR:07160'
    country_iso2 TEXT NOT NULL REFERENCES countries(iso2),
    code        TEXT NOT NULL,
    code_norm   TEXT NOT NULL,           -- normalization §5.3, digits and letters only
    place_id    TEXT REFERENCES places(id),   -- primary/most representative place
    latitude    REAL,
    longitude   REAL,
    UNIQUE (country_iso2, code)
);

CREATE TABLE postal_areas (              -- many-to-many: one code, many areas
    postal_code_id TEXT NOT NULL REFERENCES postal_codes(id),
    place_id       TEXT NOT NULL REFERENCES places(id),
    is_primary     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (postal_code_id, place_id)
) WITHOUT ROWID;

CREATE INDEX idx_postal_norm ON postal_codes(code_norm);
CREATE INDEX idx_postal_areas_place ON postal_areas(place_id);
```

Both directions from §18 are then single indexed queries:

```swift
Terra.postalCode("07160")                    // code → postal_areas → places → address
Terra.postalCodes(country: "TR", city: "Antalya")  // place → postal_areas → postal_codes
```

Real-world shapes this handles, which a naive one-to-one model gets wrong:

- **TR:** one code per district, roughly 9 000 codes, clean hierarchy.
- **US:** 41 000 ZIPs, many covering thousands of streets, some shared across a whole city. The
  `postal_codes` + `postal_areas` split is the only way to represent this without exploding `places`.
- **DE:** four- and five-digit codes, a five-digit code often containing several four-digit ones.
- **GB:** alphanumeric, outward and inward parts with a real space. `code_norm` strips the space so
  both `SW1A 1AA` and `SW1A1AA` match.
- **IE and JP:** no postal codes at all. Absent, not empty-and-broken — queries return `nil`.

Postal validation is country-specific. §30 lists invalid postal codes as a validation error, but
`^[0-9]{5}$` is wrong for GB, DE, NL, JP and dozens of others. Regexes live in a country-scoped
table generated by the pipeline, not in SDK code.

---

## 9. Search schema

§16 requires exact, prefix, fuzzy, localized, transliterated, diacritics-normalized, filtered and
ranked search. Six indexes, all built at package time.

```sql
-- 1. exact + prefix. Composite index serves both.
CREATE INDEX idx_places_norm       ON places(name_norm);
CREATE INDEX idx_places_norm_type  ON places(name_norm, type);

-- 2. fuzzy: substring, infix, typo tolerance. SQLite >= 3.34 for trigram.
CREATE VIRTUAL TABLE search_fts USING fts5(
    name_norm,
    name_norm_unaccented,
    place_id UNINDEXED,
    tokenize = 'trigram'
);

-- 3. phonetic, precomputed. No SDK computes these.
CREATE TABLE phonetic_codes (
    code     TEXT NOT NULL,          -- Double Metaphone
    place_id TEXT NOT NULL REFERENCES places(id),
    PRIMARY KEY (code, place_id)
) WITHOUT ROWID;

-- 4. structural filters
CREATE INDEX idx_places_parent ON places(parent_id, type, name_norm);
CREATE INDEX idx_places_level  ON places(country_iso2, level, name_norm);
CREATE INDEX idx_places_type   ON places(country_iso2, type, search_rank DESC, name_norm);
```

`search_rank` is a precomputed integer from the pipeline, combining population, capital status and
level. Ranking in SQL at query time cannot use an index; sorting by a precomputed integer can. §16
requires ranking and §40 requires <50 ms, and this is what makes both true.

**Trigram tokenizer and short queries.** The trigram tokenizer needs at least 3 characters to produce
a token, so `MATCH` on a one- or two-character query cannot match anything. Mitigation: the SDK routes
by query length — `length >= 3` to FTS5, otherwise to the B-tree prefix index on `places.name_norm`.
This is documented API behaviour, not a hidden fallback.

**Known limitation: CJK queries under three characters.** The three-character floor is far more
visible in Japanese, Chinese and Korean than in Latin script, because CJK place names are routinely two
or three characters. Measured on the Japanese package:

| Query | Result | Why |
|---|---|---|
| `東京都` (3) | Tokyo prefecture, Tokyo city, Chūō Ku | FTS5 trigram |
| `東京` (2) | no match | two characters cannot form a trigram |
| `大阪` (2), `京都` (2) | no match | same |

The short-query fallback is an exact-or-prefix match on `places.name_norm`, so it finds a place whose
stored name *is* `大阪` but will not find it from a two-character fragment of a longer name. This is
recorded rather than worked around: fixing it would mean substring matching on short queries, which is
a table scan, and SPEC §41 forbids that. The practical mitigation is that CJK users type the full name
in the address fields that matter, and those are three characters or more.

**Fuzzy ranking order** in `Terra.search`: exact `name_norm` match → prefix match → trigram match
ordered by Double Metaphone distance → population rank. Deterministic, and tie-broken by `id` so two
runs over the same package return the same order.

---

## 10. Language, currency, timezone reference

```sql
CREATE TABLE languages (
    code        TEXT PRIMARY KEY,      -- 'tr'
    iso639_3    TEXT NOT NULL,
    name        TEXT NOT NULL,
    name_norm   TEXT NOT NULL
);

CREATE TABLE currencies (
    code        TEXT PRIMARY KEY,      -- 'TRY', ISO 4217
    name        TEXT NOT NULL,
    symbol      TEXT,
    minor_units INTEGER NOT NULL       -- 2
);

CREATE TABLE timezones (
    id          TEXT PRIMARY KEY,      -- 'Europe/Istanbul', IANA
    name        TEXT NOT NULL,
    utc_offset_min INTEGER NOT NULL    -- advisory; DST not encoded
);
```

`utc_offset_min` is a convenience for display and sorting. It is explicitly **not** authoritative
for current time — that is `TimeZone` in Foundation and `java.time.ZoneId` in the SDKs. Storing a
fixed offset alongside an IANA identifier invites someone to use the wrong one; the column is
documented as advisory and the SDKs never return it as a `Date`.

§22 excludes exchange rates from core. There is no `rates` table and adding one is a separate
module.

---

## 11. Provenance

§28 requires `source`, `sourceVersion`, `license`, `attribution`, `generatedAt` for every dataset.
Provenance is per-row where it can differ, and per-package in the manifest.

```sql
CREATE TABLE data_sources (
    id            TEXT PRIMARY KEY,    -- 'geonames'
    name          TEXT NOT NULL,
    version       TEXT NOT NULL,
    license       TEXT NOT NULL,
    license_url   TEXT NOT NULL,
    attribution   TEXT NOT NULL,
    url           TEXT NOT NULL,
    retrieved_at  TEXT NOT NULL,
    checksum      TEXT                  -- SHA-256 of the raw download
);

-- on places:
source_id     TEXT NOT NULL REFERENCES data_sources(id),
source_row_id TEXT,                    -- original row identifier for re-sync
updated_at    TEXT NOT NULL             -- ISO 8601, dataset clock
```

`source_row_id` is what makes the pipeline re-runnable: the next sync looks up the upstream row
rather than re-deriving identity, so a renamed upstream place keeps its TerraKit ID.

**Updated timestamps come from the pipeline run's `SOURCE_DATE_EPOCH`, never the wall clock.**
[`ARCHITECTURE.md` §11](ARCHITECTURE.md#11-reproducibility) depends on this for byte-identical
rebuilds.

---

## 12. Full schema: tables and levels

The `level` column is constrained to the supported range, and every entity type declares which
levels it may occupy.

| Level | Meaning | Typical types | TR | US | DE | JP | GB |
|---|---|---|---|---|---|---|---|
| 1 | country | — | ✓ | ✓ | ✓ | ✓ | ✓ |
| 2 | top-level admin | state, province, prefecture, canton | ✓ | ✓ | ✓ | ✓ | ✓ |
| 3 | second-level | district, county, region | ✓ | ✓ | ✓ | ✓ | ✓ |
| 4 | third-level | neighborhood, borough, municipality | ✓ | ✓ | ✓ | ✓ | ✓ |
| 5 | settlement | city, town, village | ✓ | ✓ | ✓ | ✓ | ✓ |
| 6 | sub-settlement | suburb, ward | — | — | ✓ | ✓ | ✓ |
| 7 | street | street | — | — | — | ✓ | ✓ |

Street level is defined for JP and GB in this table but ships empty for every MVP package, since
streets are out of MVP scope ([`ARCHITECTURE.md` ADR-004](ARCHITECTURE.md#adr-004--geonames-cc-by-first-osm-odbl-later)).
The schema supports it so that adding streets is a data change and not a migration.

Table count for an MVP `cities` package: **17 tables, 2 virtual tables, 9 indices.** Deliberately
small. Every one of them earns its place by serving a requirement in §16, §18, §19, §20 or §30.

---

## 13. Validation rules

§30's list, each mapped to its enforcement point. Validation runs in the pipeline (blocking) and is
re-runnable by `terra validate` (non-blocking, reporting).

| §30 check | Rule | Severity |
|---|---|---|
| duplicate IDs | `id` is PK; plus cross-namespace uniqueness check | error |
| duplicate names | duplicate `name_norm` within `(parent_id, type)` → warning; identical coordinates → error | mixed |
| missing parent IDs | `parent_id` FK, and `parent.level == level - 1` | error |
| broken hierarchy | closure row count == expected `level` for every place | error |
| invalid country codes | `iso2` in ISO 3166-1 assigned set | error |
| invalid coordinates | lat ∈ [-90,90], lon ∈ [-180,180], not (0,0) unless country is legitimately null-island | error |
| invalid postal codes | country-scoped regex or explicit absence | error |
| invalid Unicode | `name` must be well-formed UTF-8, NFC-stable after NFKD, no control chars, no soft hyphen | error |
| missing required fields | `name`, `name_norm`, `level`, `country_iso2`, `source_id` non-null | error |
| orphaned entities | every place reachable from its country via closure | error |
| closure completeness | `place_closure` matches `parent_id` chain exactly | error |
| denormalization drift | `places.name_norm` == normalize(`places.name`) | error |
| fallback-normalization drift | `places.name` == normalize(`places.name_norm`) | error |
| hash-ID ratio | < 0.5% of places | warning, CI failure for MVP |
| attribution present | every `source_id` resolves to a `data_sources` row with non-empty license and attribution | error |

The last two are additions beyond §30's list, and both exist to make §29 enforceable rather than
aspirational. A package that cannot prove where its data came from is not shippable.

**Zero-error gate.** A package cannot be published with errors, only warnings. Warnings are
surfaced to the user, not hidden: `terra validate TR` prints them with counts, as in §30.

---

## 14. Versioning

Three independent version numbers, per §31.

| Version | Lives in | Semantics |
|---|---|---|
| `schemaVersion` | `meta` table in the package | integer; incremented on any backward-incompatible DDL change |
| `packageFormatVersion` | manifest | integer; the on-disk layout contract |
| `datasetVersion` | manifest | CalVer `YYYY.MM.N`, incrementing independently of SDKs |

`meta` is a key-value table queried once at open:

```sql
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- schema_version, schema_min_reader_version, generator_version,
-- dataset_version, source_date_epoch, place_count, checksum_seed
```

**Backward compatibility rule:** an SDK must read any package whose `schemaVersion` falls within its
declared `min_reader_version … max_reader_version` range. Adding a nullable column or a new table
does not increment `schemaVersion`. Dropping or renaming one does. Packages and SDKs are released
independently, so this rule is the mechanism that makes §31 hold in practice.

---

## 15. Schema DDL location

The authoritative DDL is versioned, not hand-maintained:

```text
core/schema/001_core.sql
core/schema/002_search.sql
core/schema/003_postal.sql
core/schema/004_provenance.sql
```

The pipeline builds packages by applying these files in order. Migrations are forward-only; there
are no down migrations, because packages are immutable artifacts and a rollback is served by
reinstalling the previous package version — which is also the honest answer to §32's backward
compatibility requirement.
