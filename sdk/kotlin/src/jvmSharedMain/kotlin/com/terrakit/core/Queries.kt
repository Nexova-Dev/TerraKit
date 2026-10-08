package com.terrakit.core

/**
 * Every SQL statement in the SDK, and the row mapping that goes with it.
 *
 * One object, on purpose. The alternative — a query builder, or a method per table spread across
 * the SDK — makes the SQL invisible at the call site, and the SQL *is* the contract: the CLI, the
 * Swift SDK and this one must return the same rows in the same order, and comparing that means
 * reading the three implementations side by side. [ADR-003](../docs/adr/ADR-003-search-mechanisms.md)
 * specifies raw prepared statements for the same reason.
 *
 * Pinned columns rather than `SELECT *` ([SDK_SPEC §13](../SDK_SPEC.md#13-anti-patterns)): unknown
 * columns must not become unknown SDK properties, so a package built with a newer schema still reads
 * cleanly.
 */
internal object Queries {

    /**
     * Columns every place projection reads, in a fixed order.
     *
     * A raw string, so the newlines below are part of the SQL — which is fine, SQLite ignores them.
     * They must **not** be joined with a trailing backslash the way the Swift version joins them: in
     * Kotlin's `"""` raw string a backslash is a literal character, not a line continuation, so the
     * backslashes reached SQLite and every projection failed with
     * `unrecognized token: "\"`.
     */
    const val PLACE_COLUMNS = """
        p.id, p.name, p.type, p.level, p.country_iso2, p.parent_id, p.name_norm,
        p.admin_code, p.latitude, p.longitude, p.population, p.timezone_id, p.currency_code,
        p.search_rank
    """

    fun place(row: SqlRow): Place = Place(
        id = row.text(0) ?: "",
        name = row.text(1) ?: "",
        type = PlaceType.fromWire(row.text(2)),
        level = row.int(3)?.toInt() ?: 0,
        countryCode = row.text(4) ?: "",
        parentId = row.text(5),
        nameNormalized = row.text(6) ?: "",
        administrativeCode = row.text(7),
        coordinates = coordinatesOf(row, 8, 9),
        population = row.int(10)?.toInt(),
        timezoneId = row.text(11),
        currencyCode = row.text(12),
        searchRank = row.int(13) ?: 0
    )

    private fun coordinatesOf(row: SqlRow, latitudeIndex: Int, longitudeIndex: Int): Coordinate? {
        val latitude = row.double(latitudeIndex) ?: return null
        val longitude = row.double(longitudeIndex) ?: return null
        return Coordinate(latitude, longitude)
    }

    /** The scalar columns of `countries`, copied out of the row while the cursor is still open. */
    private data class CountryRow(
        val id: String,
        val iso2: String,
        val iso3: String,
        val numericCode: String,
        val name: String,
        val continentCode: String,
        val capitalPlaceId: String?,
        val currencyCode: String,
        val phoneCode: String?,
        val internetTld: String?,
        val drivingSide: String?,
        val coordinates: Coordinate?,
    )

    fun country(pkg: Package): Country? {
        // The row is read into a value here, not returned: see [SqlHandle.select] on why a live row
        // must not escape the mapper.
        val row = pkg.database.first(
            """
            SELECT id, iso2, iso3, numeric_code, name, continent_code, capital_place_id,
                   currency_code, phone_code, internet_tld, driving_side, latitude, longitude
            FROM countries
            """
        ) { row ->
            CountryRow(
                id = row.text(0) ?: "",
                iso2 = row.text(1) ?: "",
                iso3 = row.text(2) ?: "",
                numericCode = row.text(3) ?: "",
                name = row.text(4) ?: "",
                continentCode = row.text(5) ?: "",
                capitalPlaceId = row.text(6),
                currencyCode = row.text(7) ?: "",
                phoneCode = row.text(8),
                internetTld = row.text(9),
                drivingSide = row.text(10),
                coordinates = coordinatesOf(row, 11, 12)
            )
        } ?: return null

        val iso2 = row.iso2

        // Timezones and languages are reference tables, and a country may have neither when the
        // pipeline could not populate them. Populated best-effort rather than failing: a country row
        // with no timezone list is still a useful answer.
        val timezones = pkg.database.query(
            """
            SELECT tz.id FROM country_timezones c JOIN timezones tz ON tz.id = c.timezone_id
            WHERE c.country_iso2 = ?1 ORDER BY tz.id
            """,
            listOf(SqlValue.text(iso2)),
            limit = 64
        ) { it.text(0) ?: "" }

        val languages = pkg.database.query(
            "SELECT lang FROM country_languages WHERE country_iso2 = ?1 ORDER BY lang",
            listOf(SqlValue.text(iso2)),
            limit = 512
        ) { it.text(0) ?: "" }

        return Country(
            id = row.id,
            iso2 = iso2,
            iso3 = row.iso3,
            numericCode = row.numericCode,
            name = row.name,
            continentCode = row.continentCode,
            capitalPlaceId = row.capitalPlaceId,
            currencyCode = row.currencyCode,
            phoneCode = row.phoneCode,
            internetTld = row.internetTld,
            drivingSide = DrivingSide.fromWire(row.drivingSide),
            timezones = timezones,
            languageCodes = languages,
            coordinates = row.coordinates
        )
    }

    fun cities(pkg: Package, name: String, limit: Int): List<Place> {
        val normalized = Normalization.normalize(name)
        if (normalized.isEmpty()) return emptyList()

        // The type list is bound, not interpolated. Interpolating it as `IN (city,town)` produced
        // "no such column: city" on every call — SQLite reads unquoted words as column references —
        // and the conformance battery missed it because `searchCity` is a separate query that spells
        // its literals correctly. Binding removes the whole class: there is no longer a string to
        // forget to quote.
        val types = PlaceType.SETTLEMENT_TYPES
        val placeholders = types.indices.joinToString(",") { "?" + (it + 2) }
        val limitSlot = types.size + 2

        return pkg.database.query(
            """
            SELECT $PLACE_COLUMNS FROM places p
            WHERE p.name_norm = ?1 AND p.type IN ($placeholders)
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?$limitSlot
            """,
            listOf(SqlValue.text(normalized)) +
                types.map { SqlValue.text(it.wire) } +
                listOf(SqlValue.integer(limit.coerceAtMost(QueryLimits.MAXIMUM_ENTITIES).toLong())),
            map = ::place
        )
    }

    /**
     * The three-mechanism search, merged and ranked.
     *
     * Exact, then prefix, then trigram fuzzy — the order that makes the answer total
     * ([`DATA_MODEL.md` §9](../../DATA_MODEL.md)).
     */
    fun search(packages: List<Package>, query: String, limit: Int): List<SearchResult> {
        val normalized = Normalization.normalize(query)
        if (normalized.isEmpty()) return emptyList()

        val results = mutableListOf<SearchResult>()
        val seen = mutableSetOf<String>()

        for (pkg in packages) {
            for (place in exact(pkg, normalized)) {
                append(place, MatchKind.EXACT, place.nameNormalized, results, seen)
            }
            // 2. prefix — escapes LIKE wildcards so a user's `%` is a literal per cent sign rather
            //    than a full scan.
            if (results.size < limit) {
                for (place in prefixed(pkg, normalized)) {
                    append(place, MatchKind.PREFIX, place.nameNormalized, results, seen)
                }
            }
            // 3. trigram, only for queries the tokenizer can index. A one- or two-character query
            //    cannot produce a trigram, and asking anyway would be a scan.
            //
            //    Documented behaviour, not a hidden fallback, and far more visible in CJK:
            //    `search("東京都")` works while `search("東京")` does not, because Japanese place
            //    names are routinely two characters.
            if (results.size < limit &&
                normalized.length >= Normalization.SEARCH_KEY_MINIMUM_LENGTH
            ) {
                for ((place, key) in fuzzy(pkg, normalized)) {
                    append(place, MatchKind.FUZZY, key, results, seen)
                }
            }
        }

        // The total order from DATA_MODEL.md §9. `id` last: it makes the order total, which is what
        // lets the conformance suite compare result SETS rather than counts.
        results.sortWith { lhs, rhs ->
            when {
                lhs.matchKind != rhs.matchKind -> lhs.matchKind.compareTo(rhs.matchKind)
                lhs.place.searchRank != rhs.place.searchRank ->
                    rhs.place.searchRank.compareTo(lhs.place.searchRank)
                lhs.place.id != rhs.place.id -> lhs.place.id.compareTo(rhs.place.id)
                else -> lhs.matchedOn.compareTo(rhs.matchedOn)
            }
        }

        // One row per place. Searching "Antalya" against the Turkish package matches the province
        // through five indexed names — its own, "Ili", the Basque "probintzia" and two Georgian
        // transliterations — and the CLI learned this the hard way. A search result is a place, not
        // a name variant.
        val unique = mutableListOf<SearchResult>()
        val kept = mutableSetOf<String>()
        for (result in results) {
            if (!kept.add(result.place.id)) continue
            unique += result
            if (unique.size >= limit) break
        }
        return unique
    }

    private fun append(
        place: Place,
        kind: MatchKind,
        matchedOn: String,
        results: MutableList<SearchResult>,
        seen: MutableSet<String>,
    ) {
        if (!seen.add("${place.id}|$matchedOn")) return
        results += SearchResult(place, kind, matchedOn, kind.score)
    }

    private fun exact(pkg: Package, key: String): List<Place> =
        pkg.database.query(
            """
            SELECT $PLACE_COLUMNS FROM places p
            WHERE p.name_norm = ?1
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            listOf(SqlValue.text(key), SqlValue.integer(QueryLimits.MAXIMUM_ENTITIES.toLong())),
            map = ::place
        )

    private fun prefixed(pkg: Package, key: String): List<Place> =
        pkg.database.query(
            """
            SELECT $PLACE_COLUMNS FROM places p
            WHERE p.name_norm LIKE ?1 ESCAPE '\'
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            listOf(
                SqlValue.text(escapeLike(key) + "%"),
                SqlValue.integer(QueryLimits.MAXIMUM_ENTITIES.toLong())
            ),
            map = ::place
        )

    private fun fuzzy(pkg: Package, key: String): List<Pair<Place, String>> {
        // The index is probed rather than assumed. On an Android device an old system SQLite can lack
        // the trigram tokenizer entirely, and skipping the mechanism is better than a query that
        // fails with an FTS5 error naming a tokenizer the caller cannot see.
        if (!pkg.database.capabilities().fts5) return emptyList()
        return pkg.database.query(
            """
            SELECT $PLACE_COLUMNS, f.name_norm
            FROM search_fts f JOIN places p ON p.id = f.place_id
            WHERE search_fts MATCH ?1
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            // Quoted, because an unquoted term is FTS5 query syntax: a user typing `OR` or `*` would
            // change the query rather than search for a word.
            listOf(
                SqlValue.text("\"" + key.replace("\"", "\"\"") + "\""),
                SqlValue.integer(QueryLimits.MAXIMUM_ENTITIES.toLong())
            )
        ) { row -> place(row) to (row.text(14) ?: "") }
    }

    /**
     * Escape LIKE wildcards so a user's `%` or `_` is literal.
     *
     * Without this, typing `Ant_ya` returns every six-letter place starting with "Ant" and the user
     * has no way to know why.
     */
    fun escapeLike(input: String): String = input
        .replace("\\", "\\\\")
        .replace("%", "\\%")
        .replace("_", "\\_")

    fun searchCity(packages: List<Package>, name: String, limit: Int): List<Place> {
        val normalized = Normalization.normalize(name)
        if (normalized.isEmpty()) return emptyList()
        val places = mutableListOf<Place>()
        for (pkg in packages) {
            places += pkg.database.query(
                """
                SELECT $PLACE_COLUMNS FROM places p
                WHERE p.name_norm = ?1 AND p.type IN ('city','town')
                ORDER BY p.search_rank DESC, p.id ASC
                LIMIT ?2
                """,
                listOf(
                    SqlValue.text(normalized),
                    SqlValue.integer(limit.coerceAtMost(QueryLimits.MAXIMUM_ENTITIES).toLong())
                ),
                map = ::place
            )
        }
        return places
    }

    fun postalCode(pkg: Package, code: String): PostalCode? {
        val normalized = Normalization.postalNormalize(code)
        if (normalized.isEmpty()) return null

        // Read into a value, not a live row: see [SqlHandle.select].
        val row = pkg.database.first(
            """
            SELECT code, latitude, longitude FROM postal_codes
            WHERE country_iso2 = ?1 AND code_norm = ?2
            """,
            listOf(SqlValue.text(pkg.countryCode), SqlValue.text(normalized))
        ) { Triple(it.text(0) ?: "", it.text(1), coordinatesOf(it, 1, 2)) } ?: return null

        // A code can cover several places — 6 in Türkiye span more than one district — so `areas` is
        // a list and the primary is the most specific one (DATA_MODEL.md §8).
        val areas = pkg.database.query(
            """
            SELECT $PLACE_COLUMNS
            FROM postal_areas a JOIN places p ON p.id = a.place_id
            JOIN postal_codes c ON c.id = a.postal_code_id
            WHERE c.country_iso2 = ?1 AND c.code_norm = ?2
            ORDER BY a.is_primary DESC, p.level ASC, p.name ASC
            LIMIT ?3
            """,
            listOf(
                SqlValue.text(pkg.countryCode),
                SqlValue.text(normalized),
                SqlValue.integer(QueryLimits.MAXIMUM_ENTITIES.toLong())
            ),
            map = ::place
        )

        val primaryId = pkg.database.first(
            """
            SELECT p.id
            FROM postal_areas a JOIN places p ON p.id = a.place_id
            JOIN postal_codes c ON c.id = a.postal_code_id
            WHERE c.country_iso2 = ?1 AND c.code_norm = ?2 AND a.is_primary = 1
            LIMIT 1
            """,
            listOf(SqlValue.text(pkg.countryCode), SqlValue.text(normalized))
        ) { it.text(0) }

        return PostalCode(
            code = row.first,
            countryCode = pkg.countryCode,
            primaryPlace = areas.firstOrNull { it.id == primaryId } ?: areas.firstOrNull(),
            areas = areas,
            coordinates = row.third
        )
    }

    fun postalCodes(pkg: Package, city: String?, limit: Int): List<PostalCode> {
        val bindings = mutableListOf<SqlValue>(
            SqlValue.text(pkg.countryCode),
            SqlValue.integer(limit.coerceAtMost(QueryLimits.MAXIMUM_ENTITIES).toLong())
        )
        // --city resolves through the closure table so it accepts a city, a district or a province:
        // "places inside Antalya" is the useful meaning, and resolving one normalized name would make
        // the filter brittle about which level the user named.
        val cityFilter = if (!city.isNullOrEmpty()) {
            bindings += SqlValue.text(Normalization.normalize(city))
            """
                AND c.id IN (
                    SELECT a.postal_code_id FROM postal_areas a
                    JOIN places p ON p.id = a.place_id
                    WHERE p.name_norm = ?3
                )
            """
        } else {
            ""
        }

        val rows = pkg.database.query(
            """
            SELECT c.code, c.latitude, c.longitude
            FROM postal_codes c
            WHERE c.country_iso2 = ?1 $cityFilter
            ORDER BY c.code
            LIMIT ?2
            """,
            bindings,
            limit = limit.coerceAtMost(QueryLimits.MAXIMUM_ENTITIES)
        ) { row -> (row.text(0) ?: "") to coordinatesOf(row, 1, 2) }

        return rows.map { (code, codeCoordinates) ->
            val areas = pkg.database.query(
                """
                SELECT $PLACE_COLUMNS
                FROM postal_areas a JOIN places p ON p.id = a.place_id
                WHERE a.postal_code_id = (
                    SELECT id FROM postal_codes WHERE code_norm = ?1 AND country_iso2 = ?2
                )
                ORDER BY a.is_primary DESC, p.level ASC, p.name ASC
                LIMIT ?3
                """,
                listOf(
                    SqlValue.text(Normalization.postalNormalize(code)),
                    SqlValue.text(pkg.countryCode),
                    SqlValue.integer(QueryLimits.MAXIMUM_ENTITIES.toLong())
                ),
                map = ::place
            )
            PostalCode(
                code = code,
                countryCode = pkg.countryCode,
                primaryPlace = areas.firstOrNull(),
                areas = areas,
                coordinates = codeCoordinates
            )
        }
    }

    /**
     * Places nearest a coordinate.
     *
     * Candidates come from `places_rtree`; the ordering is haversine in SDK code. The R\*Tree is a
     * candidate generator, never the answer ([`ADR-005`](../docs/adr/ADR-005-postgis-sqlite-rtree.md)):
     * a bounding-box prefilter degrades at high latitudes, exactly where data is densest.
     */
    fun nearest(packages: List<Package>, coordinate: Coordinate, limit: Int): List<Place> {
        val candidates = mutableListOf<Pair<Place, Double>>()

        for (pkg in packages) {
            val box = boundingBox(around = coordinate, kilometres = 50.0)
            val rows = pkg.database.query(
                """
                -- `places_rtree` keys on `places.rowid_int`, not on the textual id: the R*Tree stores
                -- 64-bit floats per row and a text key would be a second, larger payload in the one
                -- index that has to stay small.
                SELECT $PLACE_COLUMNS
                FROM places_rtree r JOIN places p ON p.rowid_int = r.id_rowid
                WHERE r.min_lon <= ?1 AND r.max_lon >= ?2
                  AND r.min_lat <= ?3 AND r.max_lat >= ?4
                  AND p.latitude IS NOT NULL AND p.longitude IS NOT NULL
                LIMIT ?5
                """,
                listOf(
                    SqlValue.real(box.maxLongitude),
                    SqlValue.real(box.minLongitude),
                    SqlValue.real(box.maxLatitude),
                    SqlValue.real(box.minLatitude),
                    SqlValue.integer(QueryLimits.NEAREST_CANDIDATES.toLong())
                ),
                limit = QueryLimits.NEAREST_CANDIDATES,
                map = ::place
            )
            for (place in rows) {
                val target = place.coordinates ?: continue
                candidates += place to coordinate.distanceTo(target)
            }
        }

        // Ordered here, in SDK code, rather than by the database.
        candidates.sortWith { lhs, rhs ->
            when {
                kotlin.math.abs(lhs.second - rhs.second) > 1e-9 ->
                    lhs.second.compareTo(rhs.second)
                else -> lhs.first.id.compareTo(rhs.first.id)
            }
        }
        return candidates.take(limit.coerceAtLeast(1)).map { it.first }
    }

    /**
     * A bounding box around a coordinate.
     *
     * Longitude degrees shrink with latitude, so the half-width is divided by `cos(latitude)`.
     * Without that the box is a few times too wide near the poles and too narrow at the equator for
     * the same distance — which is the concrete way a naive box prefilter fails.
     */
    private fun boundingBox(
        around: Coordinate,
        kilometres: Double,
    ): Box {
        val latitudeDelta = kilometres / 111.32
        val cosLatitude = kotlin.math.max(kotlin.math.cos(around.latitude * Math.PI / 180), 0.01)
        val longitudeDelta = kilometres / (111.32 * cosLatitude)
        return Box(
            maxOf(-90.0, around.latitude - latitudeDelta),
            minOf(90.0, around.latitude + latitudeDelta),
            maxOf(-180.0, around.longitude - longitudeDelta),
            minOf(180.0, around.longitude + longitudeDelta)
        )
    }

    private data class Box(
        val minLatitude: Double,
        val maxLatitude: Double,
        val minLongitude: Double,
        val maxLongitude: Double,
    )
}