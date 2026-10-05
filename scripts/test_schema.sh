#!/usr/bin/env bash
#
# Schema smoke test.
#
# Builds a small package from core/schema/*.sql and asserts the properties the
# SDKs depend on: FK integrity, closure-table completeness, index presence,
# R*Tree and FTS5 availability, and correct behaviour of the query patterns from
# the specification.
#
# Uses a synthetic fixture so it runs in a second with no network.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
DB="$WORK/test.sqlite"

if ! command -v sqlite3 > /dev/null; then
  echo "sqlite3 CLI not found; skipping"
  exit 0
fi

SQLITE_VERSION="$(sqlite3 --version | awk '{print $1}')"
echo "SQLite $SQLITE_VERSION"

# The FTS5 trigram tokenizer needs 3.34+ (ADR-003).
MINOR="$(echo "$SQLITE_VERSION" | cut -d. -f2)"
if (( MINOR < 34 )); then
  echo "SQLite $SQLITE_VERSION lacks the FTS5 trigram tokenizer (need >= 3.34)"
  exit 1
fi

for f in core/schema/*.sql; do
  sqlite3 "$DB" < "$f" || { echo "FAIL: applying $f"; exit 1; }
done
echo "schema applied from $(ls core/schema/*.sql | wc -l | tr -d ' ') file(s)"

sqlite3 "$DB" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO continents VALUES('AS','Asia','asia');
INSERT INTO currencies VALUES('TRY','Turkish Lira','₺',2);
INSERT INTO timezones VALUES('Europe/Istanbul','Europe/Istanbul',180);
INSERT INTO data_sources VALUES('geonames_cities500','GeoNames','2026-09-01','CC-BY-4.0','https://creativecommons.org/licenses/by/4.0/','GeoNames, CC BY 4.0','https://download.geonames.org/','2026-09-28T12:00:00Z','abc123');
INSERT INTO countries(id,iso2,iso3,numeric_code,name,name_norm,continent_code,currency_code,phone_code,internet_tld,driving_side,latitude,longitude)
  VALUES('country:TR','TR','TUR','792','Türkiye','turkiye','AS','TRY','+90','.tr','right',39.0,35.0);

-- country -> province -> district -> city -> sub-settlement, the shape real TR
-- data has. The level-4 row exists so the "mixed levels in one query" assertion
-- below actually exercises more than one depth; without it the fixture passes
-- while testing nothing.
INSERT INTO places VALUES('country:TR',1,'country',1,'TR',NULL,'Türkiye','turkiye','TR','iso3166-2',39.0,35.0,85000000,NULL,'TRY',999999,'geonames_cities500','325918','2026-10-01T00:00:00Z');
INSERT INTO places VALUES('admin:TR-07',2,'province',2,'TR','country:TR','Antalya','antalya','TR-07','iso3166-2',36.9,30.7,1600000,'Europe/Istanbul','TRY',900000,'geonames_cities500','323776','2026-10-01T00:00:00Z');
INSERT INTO places VALUES('place:geonames:7732185',3,'district',3,'TR','admin:TR-07','Alanya','alanya',NULL,'geonames',36.5444,31.9975,300000,'Europe/Istanbul','TRY',850000,'geonames_cities500','7732185','2026-10-01T00:00:00Z');
INSERT INTO places VALUES('place:geonames:323777',4,'city',3,'TR','admin:TR-07','Antalya','antalya',NULL,'geonames',36.8969,30.7133,1335000,'Europe/Istanbul','TRY',1000000,'geonames_cities500','323777','2026-10-01T00:00:00Z');
INSERT INTO places VALUES('place:geonames:13589441',5,'neighborhood',4,'TR','place:geonames:323777','Fener','fener',NULL,'geonames',36.8841,30.7052,12000,'Europe/Istanbul','TRY',400000,'geonames_cities500','13589441','2026-10-01T00:00:00Z');

-- place_names carries is_preferred AND is_short: GeoNames sets both on
-- 'de:Deutschland' (pref=1 short=1) and only short on 'de:Bundesrepublik', and a
-- country's display name is the latter's rival, not its preferred form.
INSERT INTO place_names VALUES('place:geonames:323777','tr','Latn','İstanbul','istanbul',1,0,'geonames_alternatenames');
INSERT INTO place_names VALUES('place:geonames:323777','ja','Jpan','イスタンブール','イスタンブール',1,0,'geonames_alternatenames');
INSERT INTO place_names VALUES('place:geonames:323777','en','Latn','Antalya','antalya',0,0,'geonames_primary');
INSERT INTO place_alternate_names VALUES('place:geonames:323777','Constantinople','constantinople','en','historic','geonames_alternatenames');
INSERT INTO place_alternate_names VALUES('place:geonames:323777','Βυζάντιο','βυζαντιο','el','historic','geonames_alternatenames');

-- full closure, materialized at build time (DATA_MODEL.md section 4)
INSERT INTO place_closure VALUES
 ('country:TR','country:TR',0),
 ('country:TR','admin:TR-07',1),
 ('country:TR','place:geonames:7732185',2),
 ('country:TR','place:geonames:323777',2),
 ('country:TR','place:geonames:13589441',3),
 ('admin:TR-07','admin:TR-07',0),
 ('admin:TR-07','place:geonames:7732185',1),
 ('admin:TR-07','place:geonames:323777',1),
 ('admin:TR-07','place:geonames:13589441',2),
 ('place:geonames:7732185','place:geonames:7732185',0),
 ('place:geonames:323777','place:geonames:323777',0),
 ('place:geonames:323777','place:geonames:13589441',1),
 ('place:geonames:13589441','place:geonames:13589441',0);

INSERT INTO postal_codes VALUES('postal:TR:07160','TR','07160','07160','place:geonames:7732185',36.6,32.0);
INSERT INTO postal_areas VALUES('postal:TR:07160','place:geonames:7732185',1);
INSERT INTO postal_areas VALUES('postal:TR:07160','admin:TR-07',0);

-- Every searchable name reaches the index: the place's own name, every
-- localized name, and every exonym or historic name. Indexing only the
-- alternates left Japanese search returning nothing.
INSERT INTO search_fts(name_norm,name_norm_unaccented,place_id) VALUES
 ('antalya','antalya','place:geonames:323777'),
 ('alanya','alanya','place:geonames:7732185'),
 ('turkiye','turkiye','country:TR'),
 ('fener','fener','place:geonames:13589441'),
 ('istanbul','istanbul','place:geonames:323777'),
 ('イスタンブール','イスタンブール','place:geonames:323777'),
 ('constantinople','constantinople','place:geonames:323777'),
 ('βυζαντιο','βυζαντιο','place:geonames:323777');
INSERT INTO phonetic_codes VALUES('ANTL','place:geonames:323777'),('ALN','place:geonames:7732185'),('FNR','place:geonames:13589441');

-- Every place with coordinates, because that is exactly what the validator
-- asserts (a partial R*Tree makes nearest() silently miss places).
INSERT INTO places_rtree VALUES(1,39.0,39.0,35.0,35.0);
INSERT INTO places_rtree VALUES(2,36.9,36.9,30.7,30.7);
INSERT INTO places_rtree VALUES(3,36.50,36.59,31.95,32.05);
INSERT INTO places_rtree VALUES(4,36.88,36.92,30.70,30.73);
INSERT INTO places_rtree VALUES(5,36.88,36.89,30.70,30.71);
SQL

FAILURES=0
expect() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    printf '  \033[32mok\033[0m   %s\n' "$label"
  else
    printf '  \033[31mFAIL\033[0m %s\n       got:      %s\n       expected: %s\n' \
      "$label" "$actual" "$expected"
    FAILURES=$((FAILURES + 1))
  fi
}

q() { sqlite3 "$DB" "$1"; }

echo
echo "foreign keys and integrity"
expect "foreign_key_check is clean" "$(q 'PRAGMA foreign_key_check;' | wc -l | tr -d ' ')" "0"
expect "integrity_check is ok" "$(q 'PRAGMA integrity_check;')" "ok"

echo
echo "spec section 4: hierarchy needs no UNION (ADR-001)"
expect "mixed levels under one province in a single query" \
  "$(q "SELECT COUNT(DISTINCT level) FROM place_closure c JOIN places p ON p.id=c.descendant_id WHERE c.ancestor_id='admin:TR-07' AND c.depth>0;")" \
  "2"

echo
echo "DATA_MODEL.md section 4: closure is the only ancestry source"
expect "ancestry of the city reaches the country at depth 2" \
  "$(q "SELECT depth FROM place_closure WHERE descendant_id='place:geonames:323777' AND ancestor_id='country:TR';")" \
  "2"

echo
echo "spec section 5 / 8: normalized names are indexed and exact"
expect "exact match on name_norm" \
  "$(q "SELECT name FROM places WHERE name_norm='antalya' AND type='city';")" \
  "Antalya"
expect "prefix match on the B-tree (short query path)" \
  "$(q "SELECT COUNT(*) FROM places WHERE name_norm LIKE 'ala%';")" \
  "1"

echo
echo "spec section 8: fuzzy search via FTS5 trigram"
expect "Atatürk found when the user types Ataturk (unaccented rows)" \
  "$(q "SELECT COUNT(*) FROM search_fts f JOIN places p ON p.id=f.place_id WHERE search_fts MATCH 'Antalya' AND p.type='city';")" \
  "1"

echo
echo "spec section 7 / DATA_MODEL.md 5.2: localized and alternate names"
expect "English name resolves" \
  "$(q "SELECT name FROM place_names WHERE place_id='place:geonames:323777' AND lang='en';")" \
  "Antalya"
expect "Japanese name resolves, dakuten intact" \
  "$(q "SELECT name FROM place_names WHERE place_id='place:geonames:323777' AND lang='ja';")" \
  "イスタンブール"
expect "is_preferred and is_short are independent columns" \
  "$(q "SELECT COUNT(*) FROM pragma_table_info('place_names') WHERE name IN ('is_preferred','is_short');")" \
  "2"
expect "historic exonym reaches the same place" \
  "$(q "SELECT p.name FROM place_alternate_names a JOIN places p ON p.id=a.place_id WHERE a.name_norm='constantinople';")" \
  "Antalya"
# The query string is normalized before MATCH, exactly as SDK_SPEC.md section 3
# requires. FTS5 takes the string verbatim, so passing the raw Greek with its tonos
# finds nothing while the same word normalized does — which is the whole reason
# the SDK owns the only normalization it performs.
expect "non-Latin exonym searchable once the query is normalized" \
  "$(q "SELECT p.name FROM search_fts f JOIN places p ON p.id=f.place_id WHERE search_fts MATCH 'βυζαντιο';")" \
  "Antalya"
expect "the tonos is stripped, so an unaccented Greek query also matches" \
  "$(q "SELECT COUNT(DISTINCT p.id) FROM search_fts f JOIN places p ON p.id=f.place_id WHERE search_fts MATCH 'βυζαντιο';")" \
  "1"

echo "spec section 7: localized search reaches the place"
expect "searching in Japanese finds it" \
  "$(q "SELECT COUNT(DISTINCT p.id) FROM search_fts f JOIN places p ON p.id=f.place_id WHERE search_fts MATCH 'イスタンブール';")" \
  "1"
expect "searching a historic name finds it" \
  "$(q "SELECT COUNT(DISTINCT p.id) FROM search_fts f JOIN places p ON p.id=f.place_id WHERE search_fts MATCH 'Constantinople';")" \
  "1"

echo
echo "spec section 16: phonetic precomputation (ADR-003)"
expect "phonetic lookup finds the city" \
  "$(q "SELECT p.name FROM phonetic_codes c JOIN places p ON p.id=c.place_id WHERE c.code='ANTL';")" \
  "Antalya"

echo
echo "spec section 18: postal, both directions"
expect "code -> area" \
  "$(q "SELECT p.name FROM postal_codes c JOIN postal_areas a ON a.postal_code_id=c.id JOIN places p ON p.id=a.place_id WHERE c.code_norm='07160' AND a.is_primary=1;")" \
  "Alanya"
expect "area -> code (province link included)" \
  "$(q "SELECT COUNT(DISTINCT c.code) FROM places p JOIN postal_areas a ON a.place_id=p.id JOIN postal_codes c ON c.id=a.postal_code_id WHERE p.id IN ('place:geonames:7732185','admin:TR-07');")" \
  "1"
expect "one primary per code" \
  "$(q "SELECT COUNT(*) FROM (SELECT postal_code_id FROM postal_areas WHERE is_primary=1 GROUP BY postal_code_id HAVING COUNT(*)>1);")" \
  "0"

echo
echo "spec section 19 / ADR-005: spatial index"
expect "R*Tree yields candidates for an Antalya-area box" \
  "$(q "SELECT COUNT(*) FROM places_rtree r JOIN places p ON p.rowid_int=r.id_rowid WHERE r.min_lat<=36.92 AND r.max_lat>=36.88 AND r.min_lon<=30.73 AND r.max_lon>=30.70;")" \
  "3"
expect "the city is among them (candidates are refined by haversine in the SDK)" \
  "$(q "SELECT p.name FROM places_rtree r JOIN places p ON p.rowid_int=r.id_rowid WHERE r.min_lat<=36.92 AND r.max_lat>=36.88 AND r.min_lon<=30.73 AND r.max_lon>=30.70 AND p.name_norm='antalya' AND p.type='city';")" \
  "Antalya"
expect "every place with coordinates is in the R*Tree" \
  "$(q "SELECT (SELECT COUNT(*) FROM places WHERE latitude IS NOT NULL) = (SELECT COUNT(*) FROM places_rtree);")" \
  "1"

echo
echo "spec section 20: country metadata"
expect "country timezone is IANA" "$(q "SELECT id FROM timezones WHERE id='Europe/Istanbul';")" "Europe/Istanbul"
expect "driving side" "$(q "SELECT driving_side FROM countries WHERE iso2='TR';")" "right"

echo
echo "DATA_MODEL.md section 3: identity is namespaced, not name-derived"
expect "province id is ISO 3166-2" \
  "$(q "SELECT id FROM places WHERE id='admin:TR-07';")" \
  "admin:TR-07"
expect "rename would not change an id (identity_source recorded)" \
  "$(q "SELECT identity_source FROM places WHERE id='admin:TR-07';")" \
  "iso3166-2"

echo
if [[ $FAILURES -gt 0 ]]; then
  echo
  echo "$FAILURES schema assertion(s) failed"
  exit 1
fi

echo
echo "all schema assertions passed"
