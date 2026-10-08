package com.terrakit.core

import java.io.File
import org.junit.Assume.assumeTrue
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * Cross-implementation conformance, `SDK_SPEC.md` §14.
 *
 * Runs the shared battery from `tests/integration/conformance/battery.json` against the same package
 * bytes the CLI and the Swift SDK ran, and compares **result ID sequences**, not counts. A count can
 * match while the order does not, and a user experiences that as "why is the right answer fourth".
 *
 * Two oracles, because there are three implementations:
 *
 * * `expected/cli.json` — what the Rust CLI returned. The CLI produced the packages and is therefore
 *   the reference.
 * * `expected/swift.json` — what the Swift SDK returned for the queries the CLI does not expose
 *   (`nearest`, `autocomplete`, `ancestors`, `children`, `localizedName`). Those are SDK surface; this
 *   is where §1's "same API in the same shape" is actually checked, because a divergence here is a
 *   divergence between the two SDKs rather than between an SDK and the CLI.
 *
 * Skipped, loudly, when no packages are available. `Assumptions` marks the test skipped rather than
 * passed, and the message says what to do — a suite that silently passes without its fixtures reports
 * coverage it does not have.
 *
 * ```bash
 * TERRAKIT_CONFORMANCE_PACKAGES=<dir> ./gradlew crossSdkConformance
 * ```
 */
class ConformanceTest {

    private data class BatteryQuery(
        val id: String,
        val op: String,
        val query: String?,
        val code: String?,
        val country: String?,
        val city: String?,
        val language: String?,
        val latitude: Double?,
        val longitude: Double?,
        val place: String?,
        val limit: Int?,
        val why: String?,
    )

    private val packages: File by lazy {
        val raw = System.getenv("TERRAKIT_CONFORMANCE_PACKAGES")
            ?: fail(
                """
                TERRAKIT_CONFORMANCE_PACKAGES is not set, so there are no packages to be conformant about.

                  TERRAKIT_CONFORMANCE_PACKAGES=<dir> ./gradlew crossSdkConformance
                """.trimIndent()
            )
        File(raw).also {
            assumeTrue("TERRAKIT_CONFORMANCE_PACKAGES=$raw is not a directory", it.isDirectory)
        }
    }

    private val conformanceDirectory: File by lazy {
        val root = System.getenv("TERRAKIT_REPO_ROOT")
            ?: fail("TERRAKIT_REPO_ROOT is not set; the Gradle build sets it")
        File(root, "tests/integration/conformance")
    }

    private val battery: List<BatteryQuery> by lazy {
        val document = readJson(File(conformanceDirectory, "battery.json")).jsonObject()
            ?: fail("battery.json is not a JSON object")
        val version = document.int("version") ?: fail("battery.json has no version")
        assertTrue(version >= 1, "unsupported battery version $version")
        val queries = document["queries"] as? List<*>
            ?: fail("battery.json has no 'queries' array")
        queries.mapIndexed { index, entry ->
            val map = entry.jsonObject() ?: fail("battery query #$index is not an object")
            BatteryQuery(
                id = map.string("id") ?: fail("battery query #$index has no id"),
                op = map.string("op") ?: fail("battery query #${map.string("id")} has no op"),
                query = map.string("query"),
                code = map.string("code"),
                country = map.string("country"),
                city = map.string("city"),
                language = map.string("language"),
                latitude = map.double("latitude"),
                longitude = map.double("longitude"),
                place = map.string("place"),
                limit = map.int("limit"),
                why = map.string("why")
            )
        }
    }

    private val cliExpectations: Map<String, List<String>> by lazy {
        val results = readExpectations(File(conformanceDirectory, "expected/cli.json"))
        results
    }

    private val cliUnavailable: Set<String> by lazy {
        val document = readJson(File(conformanceDirectory, "expected/cli.json")).jsonObject()
            ?: fail("expected/cli.json is not a JSON object")
        (document["results"] as? Map<*, *>)
            ?.filterValues { it.jsonObject()?.get("unavailable") != null }
            ?.keys
            ?.mapNotNull { it as? String }
            ?.toSet()
            ?: emptySet()
    }

    private val swiftExpectations: Map<String, List<String>> by lazy {
        val file = File(conformanceDirectory, "expected/swift.json")
        if (!file.isFile) {
            fail(
                """
                expected/swift.json is missing, so the queries the CLI cannot run have no oracle.

                Run the Swift SDK suite first, which records them:
                  TERRAKIT_CONFORMANCE_PACKAGES=<dir> (cd sdk/swift && swift test)
                """.trimIndent()
            )
        }
        readExpectations(file)
    }

    private fun readExpectations(file: File): Map<String, List<String>> {
        val document = readJson(file).jsonObject() ?: fail("${file.name} is not a JSON object")
        val results = document["results"] as? Map<*, *> ?: fail("${file.name} has no 'results'")
        // Built rather than `mapValues`, which widens the key to `Any?` over a `Map<*, *>`.
        val out = mutableMapOf<String, List<String>>()
        for ((key, value) in results) {
            val id = key as? String ?: fail("${file.name} has a non-string result key")
            // Each entry is an object, not a list: `{"ids": [...], "unavailable": "...", "why": "..."}`.
            // `swift.json` is the exception — it stores the bare list.
            val ids = when (value) {
                is List<*> -> value
                is Map<*, *> -> value["ids"]
                else -> null
            }
            out[id] = (ids as? List<*>)?.mapNotNull { it as? String }
                ?: fail("${file.name} has no 'ids' list for $id")
        }
        return out
    }

    private fun readJson(file: File): Any? {
        if (!file.isFile) fail("${file.path} does not exist")
        return try {
            Json.parse(file.readText())
        } catch (error: Throwable) {
            fail("${file.name} is not valid JSON: ${error.message}")
        }
    }

    // MARK: - The two comparisons

    /**
     * The CLI's recorded results, reproduced exactly.
     *
     * Not a test itself: it reads the configured resolver, and the `@Test` wrappers below call
     * [configure] first. Leaving the `@Test` here would run it unconfigured and fail on a checkout
     * with no packages, which is the "suite skips silently" outcome this file exists to avoid.
     */
    private fun matchesTheCLIRecordedExpectations() {
        val failures = mutableListOf<String>()
        var compared = 0

        for (batteryQuery in battery) {
            if (batteryQuery.id in cliUnavailable) continue
            val want = cliExpectations[batteryQuery.id]
            if (want == null) {
                failures += "${batteryQuery.id}: the CLI recorded no result ids"
                continue
            }
            val got = run(batteryQuery)
            compared++
            if (got != want) {
                failures += buildString {
                    appendLine("${batteryQuery.id} — ${batteryQuery.why ?: ""}")
                    appendLine("    CLI:    $want")
                    appendLine("    Kotlin: $got")
                }
            }
        }

        assertTrue(compared > 0, "the battery matched no queries at all")
        if (failures.isNotEmpty()) {
            fail("$failures.size/$compared divergence(s):\n\n" + failures.joinToString("\n"))
        }
    }

    /**
     * The queries the CLI cannot run, against the Swift SDK's results.
     *
     * This is the assertion that makes "both SDKs expose the same API in the same shape"
     * (`SDK_SPEC.md` §1) testable rather than aspirational. A divergence here is a difference between
     * two SDKs a user would notice by switching platform.
     *
     * Not a test itself; the `@Test` wrapper below configures first.
     */
    private fun matchesTheSwiftRecordedExpectationsForSdkOnlyQueries() {
        val failures = mutableListOf<String>()
        var compared = 0

        for (batteryQuery in battery) {
            if (batteryQuery.id !in cliUnavailable) continue
            val want = swiftExpectations[batteryQuery.id]
            if (want == null) {
                failures += "${batteryQuery.id}: Swift recorded no result and this query has no CLI equivalent"
                continue
            }
            val got = run(batteryQuery)
            compared++
            if (got != want) {
                failures += buildString {
                    appendLine("${batteryQuery.id} — ${batteryQuery.why ?: ""}")
                    appendLine("    Swift:  $want")
                    appendLine("    Kotlin: $got")
                }
            }
        }

        assertTrue(compared > 0, "no SDK-only queries were compared; the battery has drifted")
        if (failures.isNotEmpty()) {
            fail("$failures.size/$compared divergence(s) from Swift:\n\n" + failures.joinToString("\n"))
        }
    }

    // MARK: - Running the battery

    private fun run(batteryQuery: BatteryQuery): List<String> {
        val limit = batteryQuery.limit ?: 20
        return when (batteryQuery.op) {
            "search" -> Terra.search(batteryQuery.query ?: "", batteryQuery.country, limit).map { it.place.id }
            "searchCity" -> Terra.searchCity(batteryQuery.query ?: "", batteryQuery.country, limit).map { it.id }
            "country" -> countryObservations(batteryQuery)
            "postalCode" -> Terra.postalCode(batteryQuery.code ?: "", batteryQuery.country)?.areas?.map { it.id }
                ?: emptyList()
            // Codes rather than area ids: this is the reverse direction — every code covering a
            // city — and the code is the identity here. It had no runner in either SDK, so a battery
            // query was covered by nobody while the suite reported itself complete.
            "postalCodes" -> Terra.postalCodes(
                batteryQuery.country ?: fail("${batteryQuery.id} has no country"),
                batteryQuery.city,
                limit
            ).map { it.code }
            "nearest" -> Terra.nearest(
                Coordinate(batteryQuery.latitude ?: 0.0, batteryQuery.longitude ?: 0.0),
                batteryQuery.country,
                limit
            ).map { it.id }
            "autocomplete" -> Terra.autocomplete(
                batteryQuery.query ?: "",
                batteryQuery.country,
                language = batteryQuery.language?.let { Language.fromTag(it) },
                limit = limit
            ).map { it.place?.id ?: "-" }
            "ancestors" -> resolvePlace(batteryQuery).ancestors.map { it.id }
            "children" -> resolvePlace(batteryQuery).children(limit = limit).elements.map { it.id }
            "localizedName" -> listOf(localizedName(batteryQuery))
            else -> fail("the battery has an operation with no runner: '${batteryQuery.op}'")
        }
    }

    /**
     * The country row, field by field, in the order the CLI emits.
     *
     * The capital is the *name*, not the id: the CLI resolves it, and comparing an id against a name
     * would fail on every country while saying nothing about the data. A missing value is "-", because
     * the expected value must not depend on the language that generated it — Georgia has no driving
     * side in the source data, and Python's `str(None)` would be unmatchable by Kotlin's `null` or
     * Swift's `nil`.
     */
    private fun countryObservations(batteryQuery: BatteryQuery): List<String> {
        val code = batteryQuery.country ?: fail("${batteryQuery.id} has no country")
        val country = Terra.country(code) ?: return emptyList()
        val pkg = Terra.shared().requirePackage(code)

        val capitalId = country.capitalPlaceId
        val capitalName = if (capitalId == null) {
            "-"
        } else {
            pkg.database.first(
                "SELECT name FROM places WHERE id = ?1",
                listOf(SqlValue.text(capitalId)),
                map = { it.text(0) ?: "" }
            )?.takeIf { it.isNotEmpty() } ?: "-"
        }

        val placeCount = pkg.database.first(
            "SELECT count(*) FROM places WHERE country_iso2 = ?1",
            listOf(SqlValue.text(country.iso2)),
            map = { it.int(0) ?: 0L }
        ) ?: 0L

        return listOf(
            country.iso2,
            country.iso3,
            country.name,
            capitalName,
            country.currencyCode,
            country.drivingSide?.wire ?: "-",
            pkg.level,
            placeCount.toString()
        )
    }

    /**
     * Resolve the battery's symbolic placeholders.
     *
     * `"firstCity"` and `"country"` rather than a hard-coded geonamesid, because a fixture that pins
     * an id breaks the moment the pipeline rebuilds a country with different upstream data — and then
     * the suite fails for a reason that has nothing to do with conformance.
     */
    private fun resolvePlace(batteryQuery: BatteryQuery): Place {
        val code = batteryQuery.country ?: "TR"
        val token = batteryQuery.place ?: fail("${batteryQuery.id} has no place")
        val pkg = Terra.shared().requirePackage(code)

        return when (token) {
            "country" -> pkg.database.first(
                "SELECT ${Queries.PLACE_COLUMNS} FROM places p WHERE p.country_iso2 = ?1 AND p.level = 1",
                listOf(SqlValue.text(code)),
                map = Queries::place
            ) ?: fail("no country row for $code")

            "firstCity" -> pkg.database.first(
                """
                SELECT ${Queries.PLACE_COLUMNS} FROM places p
                WHERE p.country_iso2 = ?1 AND p.type IN ('city','town')
                ORDER BY p.level ASC, p.name ASC
                LIMIT 1
                """,
                listOf(SqlValue.text(code)),
                map = Queries::place
            ) ?: fail("no settlement in $code")

            else -> Hierarchy.placeById(token, pkg)
                ?: fail("no place with id $token in $code")
        }
    }

    private fun localizedName(batteryQuery: BatteryQuery): String {
        val code = batteryQuery.country ?: fail("${batteryQuery.id} has no country")
        val language = Language.fromTag(batteryQuery.language ?: "en") ?: Language.ENGLISH
        if (batteryQuery.place == "country") {
            return Terra.country(code)?.localizedName(language) ?: "-"
        }
        return resolvePlace(batteryQuery).localizedName(language)
    }

    // MARK: - Setup

    private fun configure() {
        assumeTrue(
            """
            TERRAKIT_CONFORMANCE_PACKAGES is not set, so there are no packages to be conformant about.

              TERRAKIT_CONFORMANCE_PACKAGES=<dir> ./gradlew crossSdkConformance
            """.trimIndent(),
            System.getenv("TERRAKIT_CONFORMANCE_PACKAGES") != null
        )
        Terra.configure(packages)
    }

    @Test
    fun matchesTheCLIRecordedExpectationsWithPackages() {
        configure()
        matchesTheCLIRecordedExpectations()
    }

    @Test
    fun matchesTheSwiftRecordedExpectationsWithPackages() {
        configure()
        matchesTheSwiftRecordedExpectationsForSdkOnlyQueries()
    }

    // MARK: - Behaviour the battery cannot express

    /**
     * The documented CJK limitation is asserted, so it cannot be "fixed" into a scan.
     *
     * FTS5's trigram tokenizer needs three characters. `東京都` is three and the index serves it;
     * `東京` is two and cannot be indexed (`DATA_MODEL.md` §9).
     */
    @Test
    fun theTrigramFloorIsDocumentedBehaviour() {
        configure()
        assertTrue(
            Terra.search("東京都", country = "JP", limit = 10).isNotEmpty(),
            "three characters must reach the trigram index"
        )
        val twoCharacter = Terra.search("東京", country = "JP", limit = 10)
        assertTrue(
            twoCharacter.isEmpty(),
            "two characters must return nothing rather than scan; got ${twoCharacter.map { it.place.id }}"
        )
    }

    /** A missing package is a typed error, not an empty result. */
    @Test
    fun aMissingPackageThrowsRatherThanReturningEmpty() {
        configure()
        val thrown = runCatching { Terra.country("ZZ") }.exceptionOrNull()
        assertTrue(thrown is TerraError.PackageNotInstalled, "expected PackageNotInstalled, got $thrown")
        assertEquals("ZZ", (thrown as TerraError.PackageNotInstalled).country)
    }

    /**
     * A feature the package does not provide is its own error.
     *
     * Returning an empty list would look like "no streets match" — a silent, plausible, wrong answer.
     * Streets need OSM, whose ODbL share-alike would constrain every downstream user, so they are
     * outside the MVP (ADR-004).
     */
    @Test
    fun anUnavailableFeatureThrowsRatherThanReturningEmpty() {
        configure()
        val thrown = runCatching { Terra.searchStreet("Atatürk", country = "TR") }.exceptionOrNull()
        assertTrue(thrown is TerraError.FeatureUnavailable, "expected FeatureUnavailable, got $thrown")
        assertEquals("streets", (thrown as TerraError.FeatureUnavailable).feature)
    }

    /** Ordering is total: the same query twice returns the same sequence. */
    @Test
    fun searchOrderingIsStable() {
        configure()
        val first = Terra.search("Antalya", country = "TR", limit = 20).map { it.place.id }
        val second = Terra.search("Antalya", country = "TR", limit = 20).map { it.place.id }
        assertEquals(first, second)
        assertTrue(first.isNotEmpty())
    }

    /**
     * One place, one result — even when it matches through several indexed names.
     *
     * The Turkish province matches "Antalya" through its own name, "Ili", the Basque "probintzia"
     * and Georgian transliterations. The CLI learned this the hard way; no SDK should repeat it.
     */
    @Test
    fun aPlaceMatchingSeveralNamesYieldsOneResult() {
        configure()
        val ids = Terra.search("Antalya", country = "TR", limit = 50).map { it.place.id }
        assertEquals(ids.toSet().size, ids.size, "a place appeared more than once: $ids")
    }

    /**
     * A query below the trigram floor uses only exact-or-prefix.
     *
     * `An` is two characters, so the trigram mechanism cannot run. `Ant` would be the wrong query to
     * use: three characters clears the floor, so fuzzy legitimately runs and returns Turkey itself via
     * the transliteration "an tuirc". That is correct behaviour, and asserting "everything is a
     * prefix" for it would fail for the right reason.
     */
    @Test
    fun shortQueriesUseThePrefixIndex() {
        configure()
        val results = Terra.search("An", country = "TR", limit = 20)
        assertTrue(results.isNotEmpty(), "a two-character prefix must still match")
        val names = results.map { it.place.nameNormalized }
        assertTrue(names.all { it.startsWith("an") }, "a short query must only produce prefix matches: $names")
        assertTrue(
            results.none { it.matchKind == MatchKind.FUZZY },
            "the trigram mechanism must not run below the floor"
        )
        assertTrue(
            names.contains("ankara") && names.contains("antalya"),
            "expected both Turkish cities reached by prefix, got $names"
        )
    }

    /** Three characters is exactly the floor, and it does reach the fuzzy mechanism. */
    @Test
    fun threeCharactersClearTheTrigramFloor() {
        configure()
        assertTrue(
            Terra.search("Ant", country = "TR", limit = 20).any { it.matchKind == MatchKind.FUZZY },
            "three characters is exactly the floor and must reach the fuzzy mechanism"
        )
    }

    /** Dots and wildcards in a query are literal, not syntax. */
    @Test
    fun queryWildcardsAreEscaped() {
        configure()
        val results = Terra.search("Ant_ya", country = "TR", limit = 20)
        assertTrue(
            results.all { !it.place.nameNormalized.contains('_') },
            "an underscore was treated as a single-character wildcard"
        )
    }

    /**
     * Every public entry point is reachable.
     *
     * `Terra.city(_:country:)` shipped with an unquoted `IN (city,town)`, which SQLite reads as two
     * column references, so every call failed with "no such column: city". The battery did not catch
     * it because `searchCity` is a separate query that spells its own literals correctly — two entry
     * points, one covered.
     */
    @Test
    fun everyEntryPointIsReachable() {
        configure()
        assertTrue(Terra.country("TR") != null)
        assertTrue(Terra.countries().isNotEmpty())
        assertTrue(Terra.city("Antalya", country = "TR").isNotEmpty())
        assertTrue(Terra.search("Antalya", country = "TR").isNotEmpty())
        assertTrue(Terra.searchCity("Antalya", country = "TR").isNotEmpty())
        assertTrue(Terra.postalCode("07160", country = "TR") != null)
        assertTrue(Terra.postalCodes(country = "TR").isNotEmpty())
        assertTrue(Terra.autocomplete("Ant", country = "TR").isNotEmpty())
        assertTrue(
            Terra.nearest(Coordinate(36.8969, 30.7133), country = "TR").isNotEmpty()
        )
        assertTrue(Terra.attribution().isNotEmpty())
        assertTrue(Terra.attributionString().isNotEmpty())
        assertTrue(Terra.license("TR").isNotEmpty())
        assertTrue(Terra.capabilities().countries.isNotEmpty())

        val city = Terra.city("Antalya", country = "TR").first()
        assertTrue(city.parent != null)
        assertTrue(city.ancestors.isNotEmpty())
        city.children(limit = 5)
        city.descendants(limit = 5)
        city.districts
        city.names(Language.ENGLISH)
    }

    /**
     * `city(_:country:)` is exact and narrow; `searchCity` is ranked.
     *
     * The two differ on purpose, and the difference is asserted: having one call do "cities named X"
     * and another do "cities that look like X" is how two consumers end up with different answers for
     * the same question.
     */
    @Test
    fun cityIsExactWhereSearchCityIsRanked() {
        configure()
        val exact = Terra.city("Antalya", country = "TR")
        assertTrue(exact.all { it.type.isSettlement })
        assertTrue(exact.all { it.nameNormalized == "antalya" })
        assertTrue(Terra.city("Ant", country = "TR").isEmpty(), "a prefix is not an exact match")
    }

    /**
     * Localized names resolve in every language, `SDK_SPEC.md` §7.
     *
     * The fallback chain ends in the local name, so a *missing* translation is indistinguishable from a
     * *broken* lookup if only the fallback result is ever checked. These languages are all present in
     * the Turkish package, so returning the local name would be a defect rather than the documented
     * fallback.
     */
    @Test
    fun localizedNamesResolveInEveryLanguage() {
        configure()
        val country = Terra.country("TR") ?: fail("the TR package should yield a country")
        assertEquals("Türkei", country.localizedName(Language.GERMAN))
        assertEquals("トルコ", country.localizedName(Language.JAPANESE))
        assertEquals("Türkiye", country.localizedName(Language.TURKISH))
    }

    /**
     * The everyday form outranks the formal one, wherever it is stored.
     *
     * Türkiye's English everyday name arrives as an `en/short` row in `place_alternate_names`, not in
     * `place_names`, so an implementation reading only the localized table returns
     * `Republic of Türkiye` for an English-speaking user — correct for a query, wrong for a dropdown.
     * Asserted as a property over every language the SDK offers, so it holds for all 17 and not just
     * the ones the battery names.
     */
    @Test
    fun theEverydayFormOutranksTheFormalOne() {
        configure()
        val country = Terra.country("TR") ?: fail("the TR package should yield a country")
        val place = Hierarchy.countryPlace(country) ?: fail("the TR country row should resolve")
        val pkg = Terra.shared().requirePackage("TR")

        var checked = 0
        for (language in Language.entries) {
            var everyday = pkg.database.query(
                "SELECT name FROM place_names WHERE place_id = ?1 AND lang = ?2 AND is_short = 1",
                listOf(SqlValue.text(place.id), SqlValue.text(language.tag)),
                limit = 8
            ) { it.text(0) ?: "" }
            everyday += pkg.database.query(
                "SELECT name FROM place_alternate_names WHERE place_id = ?1 AND lang = ?2 AND class = 'short'",
                listOf(SqlValue.text(place.id), SqlValue.text(language.tag)),
                limit = 8
            ) { it.text(0) ?: "" }
            if (everyday.isEmpty()) continue
            checked++
            val localized = country.localizedName(language)
            assertTrue(
                everyday.contains(localized),
                "${language.tag}: the package holds the everyday form(s) $everyday " +
                    "but localizedName returned $localized"
            )
        }
        // Only two of `Language.entries` have an everyday form for Türkiye — `tr` (the endonym) and
        // `en` (an alternate the package carries) — because GeoNames flags `is_short` for almost
        // nothing. The floor is 2, not 17: a threshold above the real count would fail the moment
        // upstream data improves, which is the wrong way for a test to break.
        assertTrue(checked >= 2, "the property was barely exercised: $checked")
    }

    /**
     * Every localized name carries the language it is actually in.
     *
     * `lang` is read back from the row rather than assumed from the tag that was queried: a package is
     * free to record a more specific tag, and reporting the assumed value would mislabel it. An
     * earlier implementation read the `is_preferred` column's integer by mistake and reported `"1"`.
     */
    @Test
    fun localizedNamesReportTheirOwnLanguage() {
        configure()
        val country = Terra.country("TR") ?: fail("the TR package should yield a country")
        val requested = country.names(Language.GERMAN)
        assertTrue(requested.isNotEmpty(), "the package holds German names for Türkiye")
        for (name in requested) {
            assertTrue(
                name.language == "de" || name.language == "de-001",
                "a name requested in German reports ${name.language}"
            )
        }
    }
}