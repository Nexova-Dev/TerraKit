//! Query engine.
//!
//! Every function here maps onto one indexed query. That mapping is the
//! contract: `SDK_SPEC.md` §3 documents it, and
//! `tests/integration/conformance/` compares result sets across the CLI, Swift and
//! Kotlin against the same package bytes. A query that changes shape here changes
//! what all three return.
//!
//! Ranking is the deterministic total order from `DATA_MODEL.md` §9:
//!
//! ```text
//! match kind (exact, then prefix, then fuzzy)
//!   → phonetic distance, ascending
//!     → search_rank, descending        [precomputed by the pipeline]
//!       → id, ascending                [the tiebreak that makes it total]
//! ```
//!
//! `search_rank` is precomputed because ranking in SQL at query time cannot use an
//! index, and spec §40 allows 50 ms for indexed search.

use rusqlite::{Connection, params};

use crate::exit::Exit;
use crate::normalize::{SEARCH_KEY_MIN_LENGTH, normalize};

/// How a result matched. Ordered: a lower variant is a better match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    Exact,
    Prefix,
    Fuzzy,
}

impl MatchKind {
    pub fn label(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Prefix => "prefix",
            MatchKind::Fuzzy => "fuzzy",
        }
    }

    fn score(self) -> f64 {
        match self {
            MatchKind::Exact => 1.0,
            MatchKind::Prefix => 0.6,
            MatchKind::Fuzzy => 0.3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub name: String,
    pub place_type: String,
    pub level: i64,
    pub country: String,
    pub population: Option<i64>,
    pub timezone: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub parent_name: Option<String>,
    pub matched_on: String,
    pub match_kind: MatchKind,
    pub search_rank: i64,
}

impl SearchResult {
    pub fn score(&self) -> f64 {
        self.match_kind.score()
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub country: Option<String>,
    pub city: Option<String>,
    pub types: Vec<String>,
    /// 0 means "unspecified" and resolves to `DEFAULT_LIMIT`.
    ///
    /// Written this way so `#[derive(Default)]` cannot silently make every caller
    /// get one result. A derived `Default` on a `usize` field yields 0, which the
    /// clamp below turns into 1 — a search that returns a single hit for
    /// everything, which reads as "there is only one Antalya".
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            country: None,
            city: None,
            types: Vec::new(),
            limit: DEFAULT_LIMIT,
        }
    }
}

/// Hard ceiling on results, `SPEC.md` §41.
///
/// Not a default — a ceiling. A caller can ask for fewer, never for more. An
/// unbounded list API is how a library ends up materializing half a dataset into
/// a `Vec`.
pub const MAX_RESULTS: usize = 1000;

const DEFAULT_LIMIT: usize = 20;

impl SearchOptions {
    /// Resolve `limit`: unspecified becomes the default, and the ceiling always
    /// applies.
    pub fn effective_limit(&self) -> usize {
        let requested = if self.limit == 0 { DEFAULT_LIMIT } else { self.limit };
        requested.clamp(1, MAX_RESULTS)
    }
}

/// Base projection shared by every result shape.
///
/// Column order is mirrored by the COL_* constants below. The fuzzy query appends
/// `f.name_norm` after these, so `matched_on` is read from COL_NAME_NORM normally
/// and from COL_FTS_MATCH for a fuzzy hit. Getting this wrong once produced a
/// `parent_name` in the `matched_on` field, which is why the positions are named
/// rather than left as bare integers.
const SELECT_PLACE: &str = "\
    p.id, p.name, p.type, p.level, p.country_iso2, p.population, p.timezone_id, \
    p.latitude, p.longitude, p.search_rank, p.name_norm, \
    (SELECT q.name FROM places q WHERE q.id = p.parent_id) AS parent_name";

const COL_ID: usize = 0;
const COL_NAME: usize = 1;
const COL_TYPE: usize = 2;
const COL_LEVEL: usize = 3;
const COL_COUNTRY: usize = 4;
const COL_POPULATION: usize = 5;
const COL_TIMEZONE: usize = 6;
const COL_LATITUDE: usize = 7;
const COL_LONGITUDE: usize = 8;
const COL_RANK: usize = 9;
const COL_NAME_NORM: usize = 10;
const COL_PARENT_NAME: usize = 11;
/// Only present on the fuzzy projection, appended after `SELECT_PLACE`.
const COL_FTS_MATCH: usize = 12;

fn row_to_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchResult> {
    Ok(SearchResult {
        id: row.get(COL_ID)?,
        name: row.get(COL_NAME)?,
        place_type: row.get(COL_TYPE)?,
        level: row.get(COL_LEVEL)?,
        country: row.get(COL_COUNTRY)?,
        population: row.get(COL_POPULATION)?,
        timezone: row.get(COL_TIMEZONE)?,
        latitude: row.get(COL_LATITUDE)?,
        longitude: row.get(COL_LONGITUDE)?,
        search_rank: row.get(COL_RANK)?,
        matched_on: row.get(COL_NAME_NORM)?,
        parent_name: row.get(COL_PARENT_NAME)?,
        match_kind: MatchKind::Exact,
    })
}

/// Search one package.
///
/// The three mechanisms from `DATA_MODEL.md` §9, in the order that makes the
/// answer total: exact, prefix, then trigram fuzzy, merged and re-sorted.
pub fn search(
    conn: &Connection,
    raw_query: &str,
    options: &SearchOptions,
) -> Result<Vec<SearchResult>, Exit> {
    let query = normalize(raw_query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let limit = options.effective_limit();
    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Parameter numbering is explicit and contiguous.
    //
    // SQLite assigns bare `?` the next free number in order of appearance, so mixing `?1`, `?2`
    // and unnamed placeholders silently collides: `--country` together with `--type` produced
    // "Wrong number of parameters passed to query" and returned nothing at all. The conformance
    // battery found it; a hand-written spot check would not have.
    //
    // Numbers are handed out only to filters that are actually present, because SQLite requires
    // the set to be contiguous -- leaving a gap for an absent `--country` makes it report "needed
    // 5" against 4 bound values.
    let mut type_filter = String::new();
    let mut extra_bindings: Vec<rusqlite::types::Value> = Vec::new();
    let mut next_slot = 2;

    if let Some(country) = options.country.as_ref().filter(|c| !c.trim().is_empty()) {
        type_filter.push_str(&format!(" AND p.country_iso2 = ?{next_slot}"));
        extra_bindings.push(rusqlite::types::Value::Text(country.to_uppercase()));
        next_slot += 1;
    }

    // --city, per SPEC section 18's `postalCodes(country:city:)`. Resolved through the closure
    // table so it accepts a city, a district or a province: "places inside Antalya" is the useful
    // meaning, and resolving one normalized name would make the flag brittle about which level
    // the user named.
    if let Some(city) = options.city.as_ref().filter(|c| !c.trim().is_empty()) {
        type_filter.push_str(&format!(
            " AND p.id IN (SELECT c.descendant_id FROM place_closure c \
             JOIN places q ON q.id = c.descendant_id \
             WHERE q.name_norm = ?{next_slot} AND q.level >= 3)"
        ));
        extra_bindings.push(rusqlite::types::Value::Text(normalize(city)));
        next_slot += 1;
    }

    if !options.types.is_empty() {
        let mut placeholders = Vec::with_capacity(options.types.len());
        for value in &options.types {
            placeholders.push(format!("?{next_slot}"));
            extra_bindings.push(rusqlite::types::Value::Text(value.clone()));
            next_slot += 1;
        }
        type_filter.push_str(&format!(" AND p.type IN ({})", placeholders.join(",")));
    }

    let base_params = |value: String| -> Vec<rusqlite::types::Value> {
        let mut out = vec![rusqlite::types::Value::Text(value)];
        out.extend(extra_bindings.clone());
        out
    };

    // --- 1. exact ----------------------------------------------------------
    let exact_sql = format!(
        "SELECT {SELECT_PLACE} FROM places p \
         WHERE p.name_norm = ?1{type_filter} \
         ORDER BY p.search_rank DESC, p.id ASC"
    );
    collect(
        conn,
        &exact_sql,
        &base_params(query.clone()),
        MatchKind::Exact,
        &mut results,
        &mut seen,
        limit,
    );

    // --- 2. prefix ---------------------------------------------------------
    if results.len() < limit {
        let prefix_sql = format!(
            "SELECT {SELECT_PLACE} FROM places p \
             WHERE p.name_norm LIKE ?1 ESCAPE '\\'{type_filter} \
             ORDER BY p.search_rank DESC, p.id ASC"
        );
        // The wildcard is escaped so a user typing `%` searches for a literal
        // per cent sign rather than turning the query into a full scan.
        let pattern = format!("{}%", escape_like(&query));
        collect(
            conn,
            &prefix_sql,
            &base_params(pattern),
            MatchKind::Prefix,
            &mut results,
            &mut seen,
            limit,
        );
    }

    // --- 3. fuzzy ----------------------------------------------------------
    // Only for queries the trigram tokenizer can index. A one- or two-character
    // query cannot produce a trigram, and asking anyway would be a scan.
    //
    // This is documented API behaviour rather than a hidden fallback, and it is
    // far more visible in CJK than in Latin script: `search("東京都")` works
    // while `search("東京")` does not, because Japanese place names are routinely
    // two characters. See DATA_MODEL.md section 9.
    if results.len() < limit && query.chars().count() >= SEARCH_KEY_MIN_LENGTH {
        let fuzzy_sql = format!(
            "SELECT {SELECT_PLACE}, f.name_norm AS matched_key \
             FROM search_fts f JOIN places p ON p.id = f.place_id \
             WHERE search_fts MATCH ?1{type_filter} \
             ORDER BY p.search_rank DESC, p.id ASC"
        );
        // The query is passed as a quoted FTS5 string. An unquoted term is FTS5
        // query syntax, so a user typing `OR` or `*` would change the query
        // rather than search for a word.
        collect(
            conn,
            &fuzzy_sql,
            &base_params(fts5_quote(&query)),
            MatchKind::Fuzzy,
            &mut results,
            &mut seen,
            limit,
        );
    }

    // --- 4. rank, then collapse to one row per place -------------------------
    //
    // Deduplication happens AFTER ranking and by place id, not during collection
    // and by (id, matched key).
    //
    // Searching "Antalya" against the Turkish package matched admin:TR-07 through
    // five different indexed names — 'antalya', 'antalya ili', 'antalya
    // probintzia', 'antalya eanangoddi', 'antalya praant' — and the earlier
    // per-key dedupe returned the same province five times. The data was right;
    // the result set was not. A search result is a place, not a name variant, and
    // `Terra.search()` in the SDKs returns places.
    //
    // Sorting first means the surviving row is the best one: the highest match
    // kind, then the highest rank. `matched_on` keeps recording which name
    // matched, which is what explains a fuzzy hit.
    sort_total(&mut results);

    let mut unique: Vec<SearchResult> = Vec::with_capacity(results.len());
    let mut kept_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for result in results {
        if kept_ids.insert(result.id.clone()) {
            unique.push(result);
        }
    }
    unique.truncate(limit);
    Ok(unique)
}

/// The total order from `DATA_MODEL.md` §9.
///
/// `id` last: it makes the order total, which is what lets the cross-SDK
/// conformance suite compare exact result sets rather than just counts.
fn sort_total(results: &mut [SearchResult]) {
    results.sort_by(|a, b| {
        a.match_kind
            .cmp(&b.match_kind)
            .then_with(|| b.search_rank.cmp(&a.search_rank))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.matched_on.cmp(&b.matched_on))
    });
}

#[allow(clippy::too_many_arguments)]
/// Run one search query and append its rows.
///
/// `params` must be in placeholder order: the search key, then country, then city, then the type
/// list — the numbering `search` assigns. Every value the SQL contains is bound from here, so the
/// two cannot drift apart, which is the failure the previous scheme had.
fn collect(
    conn: &Connection,
    sql: &str,
    params: &[rusqlite::types::Value],
    kind: MatchKind,
    results: &mut Vec<SearchResult>,
    seen: &mut std::collections::HashSet<String>,
    limit: usize,
) {
    let mut statement = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("terrakit: query failed: {e}");
            return;
        }
    };

    let bound: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    let rows = statement.query_map(bound.as_slice(), |row| {
        let mut result = row_to_result(row)?;
        result.match_kind = kind;
        if kind == MatchKind::Fuzzy {
            // The matched key overrides the place's own normalized name, so the
            // result says *why* it matched. Without this, searching
            // "Constantinople" returns İstanbul with no indication that it was
            // the historic name that matched rather than the current one.
            if let Ok(matched) = row.get::<usize, String>(COL_FTS_MATCH) {
                result.matched_on = matched;
            }
        }
        Ok(result)
    });

    match rows {
        Ok(iter) => {
            for row in iter.flatten() {
                if results.len() >= limit {
                    return;
                }
                // Suppressed here so one place does not fill the whole limit
                // before ranking. The authoritative dedupe is by place id after
                // sorting; this is only a work-avoidance guard.
                if seen.insert(row.id.clone()) {
                    results.push(row);
                }
            }
        }
        Err(e) => eprintln!("terrakit: query failed: {e}"),
    }
}

/// Escape LIKE wildcards so a user's `%` or `_` is literal.
///
/// Without this, typing `Ant_ya` returns every six-letter place starting with
/// "Ant" and the user has no way to know why.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Quote a term for FTS5 so it is matched literally.
///
/// FTS5 treats bare words as query syntax: `OR`, `NEAR` and `*` change the
/// query rather than being searched for. A user typing `AT&T` would otherwise get
/// a syntax error instead of results.
fn fts5_quote(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

/// Country metadata, `terra info`.
#[derive(Debug, Clone)]
pub struct CountryInfo {
    pub iso2: String,
    pub iso3: String,
    pub numeric_code: String,
    pub name: String,
    pub continent: String,
    pub capital: Option<String>,
    pub currency: Option<String>,
    pub phone: Option<String>,
    pub tld: Option<String>,
    pub driving_side: Option<String>,
    pub timezones: Vec<String>,
    pub languages: Vec<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub places: i64,
    pub postal_codes: i64,
    pub place_names: i64,
    pub alternate_names: i64,
    pub languages_count: i64,
}

pub fn country_info(conn: &Connection, iso2: &str) -> Result<CountryInfo, Exit> {
    let mut info = conn
        .query_row(
            "SELECT iso2, iso3, numeric_code, name, continent_code, capital_place_id, \
                    currency_code, phone_code, internet_tld, driving_side, latitude, longitude \
             FROM countries WHERE iso2 = ?1",
            params![iso2.to_uppercase()],
            |row| {
                let capital_id: Option<String> = row.get(5)?;
                let capital: Option<String> = capital_id.and_then(|id| {
                    conn.query_row(
                        "SELECT name FROM places WHERE id = ?1",
                        params![id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                });
                Ok(CountryInfo {
                    iso2: row.get(0)?,
                    iso3: row.get(1)?,
                    numeric_code: row.get(2)?,
                    name: row.get(3)?,
                    continent: row.get(4)?,
                    capital,
                    currency: row.get(6)?,
                    phone: row.get(7)?,
                    tld: row.get(8)?,
                    driving_side: row.get(9)?,
                    latitude: row.get(10)?,
                    longitude: row.get(11)?,
                    timezones: Vec::new(),
                    languages: Vec::new(),
                    places: 0,
                    postal_codes: 0,
                    place_names: 0,
                    alternate_names: 0,
                    languages_count: 0,
                })
            },
        )
        .map_err(|_| Exit::NotFound)?;

    // country_timezones and country_languages are populated by the pipeline's
    // reference tables, which are keyed on the country table. A country package
    // with an empty timezone reference is still usable, so a missing table is not
    // fatal here — `doctor` reports it.
    info.timezones = strings(
        conn,
        "SELECT tz.id FROM country_timezones c JOIN timezones tz ON tz.id = c.timezone_id \
         WHERE c.country_iso2 = ?1 ORDER BY tz.id",
        &info.iso2,
    );
    if info.timezones.is_empty() {
        info.timezones = strings(
            conn,
            "SELECT DISTINCT timezone_id FROM places \
             WHERE country_iso2 = ?1 AND timezone_id IS NOT NULL ORDER BY timezone_id",
            &info.iso2,
        );
    }
    info.languages = strings(
        conn,
        "SELECT lang FROM country_languages WHERE country_iso2 = ?1 ORDER BY lang",
        &info.iso2,
    );

    info.places = scalar(conn, "SELECT COUNT(*) FROM places WHERE country_iso2 = ?1", &info.iso2);
    info.postal_codes = scalar(conn, "SELECT COUNT(*) FROM postal_codes WHERE country_iso2 = ?1", &info.iso2);
    info.place_names = total(conn, "SELECT COUNT(*) FROM place_names");
    info.alternate_names = total(conn, "SELECT COUNT(*) FROM place_alternate_names");
    info.languages_count = conn
        .query_row("SELECT COUNT(DISTINCT lang) FROM place_names", [], |r| r.get(0))
        .unwrap_or(0);

    Ok(info)
}

fn strings(conn: &Connection, sql: &str, arg: &str) -> Vec<String> {
    let mut statement = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    statement
        .query_map(params![arg], |row| row.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

fn scalar(conn: &Connection, sql: &str, arg: &str) -> i64 {
    conn.query_row(sql, params![arg], |row| row.get(0)).unwrap_or(0)
}

/// Whole-package counts, no filter.
fn total(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap_or(0)
}

/// Postal codes, both directions (`SPEC.md` §18).
#[derive(Debug, Clone)]
pub struct PostalHit {
    pub code: String,
    pub place_name: Option<String>,
    /// The primary place's id. Carried beside the name so JSON consumers compare ids.
    pub place_id: Option<String>,
    pub place_type: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub areas: Vec<(String, String)>,
}

pub fn postal_code(conn: &Connection, country: &str, raw: &str) -> Result<PostalHit, Exit> {
    let normalized = crate::normalize::postal_normalize(raw);
    let country = country.to_uppercase();

    let mut hit = conn
        .query_row(
            "SELECT code, place_id, latitude, longitude FROM postal_codes \
             WHERE country_iso2 = ?1 AND code_norm = ?2",
            params![country, normalized],
            |row| {
                let place_id: Option<String> = row.get(1)?;
                let resolved: Option<(String, String)> = place_id.and_then(|id| {
                    conn.query_row(
                        "SELECT name, type FROM places WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .ok()
                });
                let place_name = resolved.as_ref().map(|(name, _)| name.clone());
                let place_id = resolved.as_ref().map(|(name, _)| name.clone());
                let place_type = resolved.map(|(_, kind)| kind);
                Ok(PostalHit {
                    code: row.get(0)?,
                    place_name,
                    place_id,
                    place_type,
                    latitude: row.get(2)?,
                    longitude: row.get(3)?,
                    areas: Vec::new(),
                })
            },
        )
        .map_err(|_| Exit::NotFound)?;

    // A code can cover several places — 6 in Türkiye span more than one district —
    // so `areas` is a list and `place_name` is only the primary. See
    // `DATA_MODEL.md` §8.
    let mut statement = conn
        .prepare(
            "SELECT p.id, p.name FROM postal_areas a JOIN places p ON p.id = a.place_id \
             WHERE a.postal_code_id = (SELECT id FROM postal_codes \
             WHERE country_iso2 = ?1 AND code_norm = ?2) \
             ORDER BY a.is_primary DESC, p.level ASC, p.name ASC",
        )
        .map_err(|e| {
            eprintln!("terrakit: {e}");
            Exit::Failure
        })?;
    hit.areas = statement
        .query_map(params![country, normalized], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(0)?))
        })
        .map_err(|e| {
            eprintln!("terrakit: {e}");
            Exit::Failure
        })?
        .flatten()
        .collect();

    Ok(hit)
}

/// Count of entities, for `doctor` and `validate`.
pub struct PackageCounts {
    pub places: i64,
    pub closure: i64,
    pub postal_codes: i64,
    pub postal_areas: i64,
    pub place_names: i64,
    pub alternate_names: i64,
    pub search_fts: i64,
    pub phonetic: i64,
    pub rtree: i64,
    pub sources: i64,
}

pub fn counts(conn: &Connection) -> PackageCounts {
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    PackageCounts {
        places: one("SELECT COUNT(*) FROM places"),
        closure: one("SELECT COUNT(*) FROM place_closure"),
        postal_codes: one("SELECT COUNT(*) FROM postal_codes"),
        postal_areas: one("SELECT COUNT(*) FROM postal_areas"),
        place_names: one("SELECT COUNT(*) FROM place_names"),
        alternate_names: one("SELECT COUNT(*) FROM place_alternate_names"),
        search_fts: one("SELECT COUNT(*) FROM search_fts"),
        phonetic: one("SELECT COUNT(*) FROM phonetic_codes"),
        rtree: one("SELECT COUNT(*) FROM places_rtree"),
        sources: one("SELECT COUNT(*) FROM data_sources"),
    }
}

/// Whether the SQLite build has everything the data model needs.
///
/// Checked rather than assumed: the FTS5 trigram tokenizer requires SQLite >= 3.34
/// and is not compiled into every build. A package opened on a build without it
/// would fail on the first search rather than explaining why.
pub fn sqlite_capabilities(conn: &Connection) -> Vec<(String, bool)> {
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap_or_default();

    // Probed rather than assumed. The FTS5 trigram tokenizer needs SQLite >= 3.34
    // and is not compiled into every build, so a package opened on a build without
    // it would otherwise fail on the first search instead of explaining why.
    let trigram = conn
        .execute_batch("CREATE VIRTUAL TABLE temp.probe_fts USING fts5(x, tokenize='trigram')")
        .is_ok();
    let rtree = conn
        .execute_batch("CREATE VIRTUAL TABLE temp.probe_rtree USING rtree(id,a,b,c,d)")
        .is_ok();

    vec![
        (format!("SQLite {version}"), true),
        ("fts5 trigram".into(), trigram),
        ("rtree".into(), rtree),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("Ant_ya"), "Ant\\_ya");
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like("Antalya"), "Antalya");
    }

    #[test]
    fn fts5_terms_are_quoted() {
        // An unquoted OR would be FTS5 syntax and change the query.
        assert_eq!(fts5_quote("OR"), "\"OR\"");
        assert_eq!(fts5_quote("AT&T"), "\"AT&T\"");
        assert_eq!(fts5_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn match_kind_orders_best_first() {
        assert!(MatchKind::Exact < MatchKind::Prefix);
        assert!(MatchKind::Prefix < MatchKind::Fuzzy);
        assert!(MatchKind::Exact.score() > MatchKind::Fuzzy.score());
    }

    #[test]
    fn result_limit_is_clamped_never_unbounded() {
        let opts = SearchOptions {
            limit: 100_000,
            ..Default::default()
        };
        assert_eq!(opts.effective_limit(), MAX_RESULTS);

        // 0 is "unspecified", not "give me nothing".
        let unspecified = SearchOptions {
            limit: 0,
            ..Default::default()
        };
        assert_eq!(unspecified.effective_limit(), DEFAULT_LIMIT);
        assert_eq!(
            SearchOptions::default().effective_limit(),
            DEFAULT_LIMIT,
            "Default must yield the default limit, not the clamp of 0"
        );
        assert_eq!(
            SearchOptions { limit: 5, ..Default::default() }.effective_limit(),
            5
        );
    }

    #[test]
    fn ranking_is_a_total_order() {
        let mut rows = vec![
            SearchResult {
                id: "b".into(),
                match_kind: MatchKind::Fuzzy,
                search_rank: 10,
                matched_on: "x".into(),
                ..blank()
            },
            SearchResult {
                id: "a".into(),
                match_kind: MatchKind::Exact,
                search_rank: 1,
                matched_on: "x".into(),
                ..blank()
            },
            SearchResult {
                id: "c".into(),
                match_kind: MatchKind::Prefix,
                search_rank: 5,
                matched_on: "x".into(),
                ..blank()
            },
        ];
        sort_total(&mut rows);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c", "b"],
            "exact before prefix before fuzzy, regardless of rank"
        );
    }

    #[test]
    fn one_place_matching_several_names_yields_one_result() {
        // Regression: admin:TR-07 matches "Antalya" through five indexed names
        // (its own, 'İli', the Basque 'probintzia', and two Georgian
        // transliterations). Deduping by (id, matched key) returned the same
        // province five times.
        let mut rows: Vec<SearchResult> = [
            ("admin:TR-07", "antalya", MatchKind::Exact),
            ("place:geonames:323777", "antalya", MatchKind::Exact),
            ("admin:TR-07", "antalya ili", MatchKind::Fuzzy),
            ("admin:TR-07", "antalya probintzia", MatchKind::Fuzzy),
        ]
        .iter()
        .map(|(id, matched, kind)| SearchResult {
            id: (*id).into(),
            matched_on: (*matched).into(),
            match_kind: *kind,
            ..blank()
        })
        .collect();

        sort_total(&mut rows);
        let mut unique: Vec<SearchResult> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            if seen.insert(row.id.clone()) {
                unique.push(row);
            }
        }

        assert_eq!(unique.len(), 2, "one row per place, not per name");
        assert_eq!(unique[0].id, "admin:TR-07");
        assert_eq!(
            unique[0].matched_on, "antalya",
            "the surviving row must be the best match, and keep the name that matched"
        );
    }

    #[test]
    fn equal_rows_are_broken_by_id_so_the_order_is_total() {
        // Two packages, or one package queried twice, must produce the same order.
        let mut rows = vec![
            SearchResult { id: "z".into(), search_rank: 5, matched_on: "m".into(), ..blank() },
            SearchResult { id: "a".into(), search_rank: 5, matched_on: "m".into(), ..blank() },
        ];
        sort_total(&mut rows);
        assert_eq!(rows[0].id, "a");

        // And the order must not flip when the input order does.
        rows.reverse();
        sort_total(&mut rows);
        assert_eq!(rows[0].id, "a");
    }

    fn blank() -> SearchResult {
        SearchResult {
            id: String::new(),
            name: String::new(),
            place_type: String::new(),
            level: 0,
            country: String::new(),
            population: None,
            timezone: None,
            latitude: None,
            longitude: None,
            parent_name: None,
            matched_on: String::new(),
            match_kind: MatchKind::Fuzzy,
            search_rank: 0,
        }
    }
}
