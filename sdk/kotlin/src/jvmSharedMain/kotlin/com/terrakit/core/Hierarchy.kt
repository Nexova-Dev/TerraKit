package com.terrakit.core

/**
 * Hierarchy traversal, `SDK_SPEC.md` §5.
 *
 * **Zero recursive queries.** Every package ships `place_closure` fully materialized, so ancestry is
 * one indexed join ([`ARCHITECTURE.md` §4](../../ARCHITECTURE.md)). A Turkish neighbourhood's
 * ancestors return `[neighborhood, district, province, country]` in four indexed lookups, not a
 * recursive CTE with an unbounded depth guess.
 *
 * **Keyset pagination, never `OFFSET`.** `OFFSET 500000` walks half a million rows and throws them
 * away; street lists and large administrative subdivisions hit that immediately.
 */
val Place.parent: Place?
    get() = parentId?.let { Hierarchy.placeById(it, packageFor()) }

/** Every ancestor, root-first, including self. */
val Place.ancestors: List<Place>
    get() = Hierarchy.ancestors(this)

/**
 * The immediate parent's name, or `null` at the top of the hierarchy.
 *
 * Kept separate from [parent] because autocomplete needs the name without materializing the whole
 * parent row.
 */
val Place.parentName: String?
    get() = Hierarchy.parentName(this)

/** One page of direct children, keyset-paginated. Pass [Page.nextCursor] back as `after`. */
fun Place.children(after: String? = null, limit: Int = 100): Page<Place> =
    Hierarchy.children(this, after, limit)

/** Every descendant, ordered by level then name. Bounded by `limit`. */
fun Place.descendants(limit: Int = QueryLimits.MAXIMUM_ENTITIES): List<Place> =
    Hierarchy.descendants(this, limit)

/** Direct children of type [PlaceType.DISTRICT]. */
val Place.districts: List<Place>
    get() = children(limit = QueryLimits.MAXIMUM_ENTITIES).elements.filter { it.type == PlaceType.DISTRICT }

/** Every localized name for this place, ordered for display. */
fun Place.names(language: Language): List<PlaceName> = Localization.placeNames(this, language)

/**
 * The best display name in a language, or the place's own name.
 *
 * Missing translations are normal, not an error (spec §7), so the place's own name is the floor
 * rather than an error. Within a language the everyday form wins over the formal one: [names] is
 * already ordered by [PlaceNameClass.displayRank], so the first entry is the answer and there is no
 * second rule here to get out of step with it.
 */
fun Place.localizedName(language: Language): String = names(language).firstOrNull()?.name ?: name

/** The country's place row, which carries the hierarchy anchor at level 1. */
val Country.place: Place?
    get() = Hierarchy.countryPlace(this)

/** Every localized name for this country. */
fun Country.names(language: Language): List<PlaceName> =
    place?.let { Localization.placeNames(it, language) } ?: emptyList()

/**
 * The best display name in a language.
 *
 * `country("TR").name` is `Türkiye` rather than GeoNames' English `Turkey`, because the pipeline
 * promotes a display name into the country row. `localizedName` returns the name in the requested
 * language where the package holds one, and the promoted endonym otherwise — the fallback chain's
 * floor (`SDK_SPEC.md` §7).
 */
fun Country.localizedName(language: Language): String = names(language).firstOrNull()?.name ?: name

internal object Hierarchy {

    fun placeById(id: String, pkg: Package): Place? = pkg.database.first(
        "SELECT ${Queries.PLACE_COLUMNS} FROM places p WHERE p.id = ?1",
        listOf(SqlValue.text(id)),
        map = Queries::place
    )

    fun countryPlace(country: Country): Place? {
        val pkg = Terra.shared().packageFor(country.iso2) ?: return null
        return pkg.database.first(
            """
            SELECT ${Queries.PLACE_COLUMNS} FROM places p
            WHERE p.country_iso2 = ?1 AND p.level = 1
            """,
            listOf(SqlValue.text(country.iso2)),
            map = Queries::place
        )
    }

    fun ancestors(place: Place): List<Place> = with(place.packageFor()) {
        database.query(
            """
            SELECT ${Queries.PLACE_COLUMNS}
            FROM place_closure c JOIN places p ON p.id = c.ancestor_id
            WHERE c.descendant_id = ?1
            ORDER BY c.depth DESC
            """,
            listOf(SqlValue.text(place.id)),
            // Levels are 1...7, so 8 rows including self is the exact bound rather than a guess.
            limit = 8,
            map = Queries::place
        )
    }

    fun descendants(place: Place, limit: Int): List<Place> = with(place.packageFor()) {
        database.query(
            """
            SELECT ${Queries.PLACE_COLUMNS}
            FROM place_closure c JOIN places p ON p.id = c.descendant_id
            WHERE c.ancestor_id = ?1 AND c.depth > 0
            ORDER BY p.level ASC, p.name ASC
            """,
            listOf(SqlValue.text(place.id)),
            limit = limit.coerceAtMost(QueryLimits.MAXIMUM_ENTITIES),
            map = Queries::place
        )
    }

    fun children(place: Place, after: String?, limit: Int): Page<Place> = with(place.packageFor()) {
        val capped = limit.coerceIn(1, QueryLimits.MAXIMUM_ENTITIES)
        // Keyset on (name, id) rather than (id): the ordering is by name, so paging on id would skip
        // or repeat rows whenever two children share a name — which real data does.
        val sql = if (after == null) {
            """
            SELECT ${Queries.PLACE_COLUMNS} FROM places p
            WHERE p.parent_id = ?1
            ORDER BY p.name ASC, p.id ASC
            LIMIT ?2
            """
        } else {
            """
            SELECT ${Queries.PLACE_COLUMNS} FROM places p
            WHERE p.parent_id = ?1 AND (p.name, p.id) > (?2, ?2)
            ORDER BY p.name ASC, p.id ASC
            LIMIT ?3
            """
        }
        val bindings = if (after == null) {
            listOf(SqlValue.text(place.id), SqlValue.integer((capped + 1).toLong()))
        } else {
            listOf(SqlValue.text(place.id), SqlValue.text(after), SqlValue.integer((capped + 1).toLong()))
        }

        val rows = database.query(sql, bindings, limit = capped + 1, map = Queries::place)
        if (rows.size <= capped) return@with Page(rows, null)
        val page = rows.take(capped)
        Page(page, page.lastOrNull()?.id)
    }

    fun parentName(place: Place): String? = with(place.packageFor()) {
        val parentId = place.parentId ?: return@with null
        database.first(
            "SELECT name FROM places WHERE id = ?1",
            listOf(SqlValue.text(parentId))
        ) { it.text(0) }
    }
}

/**
 * The package a place came from.
 *
 * Throws if the package has since vanished from disk. That is deliberate: a [Place] only ever comes
 * out of a query against a package, so its absence means the resolver was reconfigured or the file
 * was deleted underneath the caller — a state no honest return value describes. Silently degrading
 * a hierarchy walk to an empty list there would turn a deployment problem into "this place has no
 * parent", which is much harder to notice.
 */
internal fun Place.packageFor(): Package = Terra.shared().packageFor(countryCode)
    ?: throw TerraError.PackageNotInstalled(countryCode)

/**
 * Localized names, `SDK_SPEC.md` §7.
 *
 * Two sources, and the distinction matters:
 *
 * * `place_names` — localized, with the preferred/short flags GeoNames provides
 * * `place_alternate_names` — exonyms, historic names and transliterations
 *
 * `place_alternate_names` is why `localizedName(Language.JAPANESE)` on İstanbul returns
 * `イスタンブール`, why an English UI shows `Türkiye` rather than `Republic of Türkiye` (the everyday
 * form arrives as an `en/short` alternate), and why searching `Constantinople` finds Istanbul.
 */
internal object Localization {

    /**
     * Names for a place in the requested language, ordered for display.
     *
     * **The chain stops at the first rung that answers.** `SDK_SPEC.md` §7 defines the fallback as
     * `requested language -> locale fallback -> "en" -> local name`, and a chain that kept walking
     * past the first answer is not a fallback chain — it is a union. Merging the rungs and sorting by
     * name class made the English `short` name outrank the German `preferred` one, so
     * `localizedName(Language.GERMAN)` on Türkiye returned `Türkiye` while the package held `Türkei`
     * one rung earlier.
     */
    fun placeNames(place: Place, language: Language): List<PlaceName> {
        val pkg = place.packageFor()

        for (tag in language.fallbackChain) {
            val names = mutableListOf<PlaceName>()

            names += pkg.database.query(
                """
                SELECT lang, name, script, is_preferred, is_short
                FROM place_names
                WHERE place_id = ?1 AND lang = ?2
                """,
                listOf(SqlValue.text(place.id), SqlValue.text(tag)),
                limit = 32
            ) { row ->
                val isShort = (row.int(4) ?: 0L) == 1L
                val isPreferred = (row.int(3) ?: 0L) == 1L
                PlaceName(
                    name = row.text(1) ?: "",
                    script = row.text(2),
                    // Read `lang` back rather than assuming the tag: a package is free to record a
                    // more specific tag than the one queried, and reporting the assumed value would
                    // mislabel it.
                    language = row.text(0) ?: tag,
                    // Both flags can be set; short wins because it is the everyday name.
                    nameClass = if (isShort) PlaceNameClass.SHORT else {
                        if (isPreferred) PlaceNameClass.PREFERRED else PlaceNameClass.SHORT
                    }
                )
            }

            // Alternates in the same language: exonyms, historic names and transliterations. Included
            // because a caller asking for a language wants what that language calls the place.
            // `lang` is nullable there, and a name with no recorded language is not a localized
            // name — it is reachable through search, not through this.
            names += pkg.database.query(
                """
                SELECT lang, name, class FROM place_alternate_names
                WHERE place_id = ?1 AND lang = ?2
                """,
                listOf(SqlValue.text(place.id), SqlValue.text(tag)),
                limit = 32
            ) { row ->
                PlaceName(
                    name = row.text(1) ?: "",
                    script = null,
                    language = row.text(0) ?: tag,
                    nameClass = PlaceNameClass.fromWire(row.text(2))
                )
            }

            if (names.isEmpty()) continue

            // Deduplicated by text, then ordered for display. The class decides and the text breaks
            // ties, so the order is total rather than dependent on which table the row came from.
            val seen = mutableSetOf<String>()
            return names
                .filter { it.name.isNotEmpty() && seen.add(it.name) }
                .sortedWith { lhs, rhs ->
                    when {
                        lhs.nameClass != rhs.nameClass ->
                            lhs.nameClass.displayRank.compareTo(rhs.nameClass.displayRank)
                        else -> lhs.name.compareTo(rhs.name)
                    }
                }
        }

        // Every rung of the chain was empty. Missing translations are normal, so this is an empty
        // result rather than an error, and the caller's own name is the floor.
        return emptyList()
    }
}
