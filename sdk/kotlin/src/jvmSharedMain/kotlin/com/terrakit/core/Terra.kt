package com.terrakit.core

import java.io.File

/**
 * The `Terra` namespace — the entry points spec §50 and §51 name.
 *
 * Each is a thin wrapper over one indexed query, so the query plans in `SDK_SPEC.md` §3 are the
 * contract rather than an implementation detail.
 *
 * **Offline by construction.** No query path contains a network branch
 * ([`ARCHITECTURE.md` §1](../../ARCHITECTURE.md)). The SDK does not download:
 * `Terra.country("JP")` on a machine with no JP package throws `PackageNotInstalled`. Downloading is
 * the CLI's job.
 */
object Terra {

    /** This SDK's version, reported in `SchemaTooNew` so the error names what is needed. */
    const val SDK_VERSION = "1.0.0"

    /** [SDK_VERSION] as a comparable number, for `minReaderVersion` checks. */
    const val SDK_VERSION_NUMBER = 1_000_000

    private val lock = Any()
    private var resolver: PackageResolver? = null

    /** Use an explicit package directory instead of discovery. */
    @JvmStatic
    @JvmOverloads
    fun configure(packageDirectory: File, trustedKeyIds: Set<String> = emptySet()) =
        configure(listOf(packageDirectory), trustedKeyIds)

    /** Use explicit package roots, in priority order. */
    @JvmStatic
    @JvmOverloads
    fun configure(roots: List<File>, trustedKeyIds: Set<String> = emptySet()) {
        require(roots.isNotEmpty()) { "no pkg roots were given" }
        install(PackageResolver(roots).also { it.trustedKeyIds = trustedKeyIds })
    }

    /**
     * Open packages by discovery: the application bundle first, then `.terrakit/packages/`.
     *
     * Bundle first because that is what a shipped app has. Resolution is one `stat` per country, done
     * once and cached.
     */
    @JvmStatic
    @JvmOverloads
    fun open(
        bundleDirectory: File? = defaultBundleDirectory(),
        workingDirectory: File = File("").absoluteFile,
    ): PackageResolver = install(PackageResolver(defaultRoots(bundleDirectory, workingDirectory)))

    private fun install(resolver: PackageResolver): PackageResolver {
        synchronized(lock) { this.resolver = resolver }
        return resolver
    }

    /**
     * The active resolver, opening by discovery if `open()` has not been called.
     *
     * Auto-opening is what makes `Terra.countries()` work in a script with no setup, which is the
     * common case. It is cached, so the discovery walk happens once.
     */
    @JvmStatic
    fun shared(): PackageResolver {
        synchronized(lock) { resolver }?.let { return it }
        return open()
    }

    /** The default search roots, in SDK_SPEC §2 order. */
    fun defaultRoots(bundleDirectory: File?, workingDirectory: File): List<File> {
        val roots = mutableListOf<File>()

        // 2. the application bundle
        if (bundleDirectory != null) {
            for (name in listOf("TerraKit", "terrakit")) {
                val candidate = File(bundleDirectory, name)
                if (candidate.isDirectory) roots += candidate
            }
        }

        // 3. .terrakit/packages, walking up from the working directory
        var current: File? = workingDirectory.absoluteFile
        repeat(8) {
            val directory = current ?: return@repeat
            val candidate = File(directory, ".terrakit/packages")
            if (candidate.isDirectory) roots += candidate
            current = directory.parentFile
        }
        return roots
    }

    private fun defaultBundleDirectory(): File? {
        val codeSource = Terra::class.java.protectionDomain?.codeSource ?: return null
        val location = codeSource.location?.toURI()?.let { File(it) } ?: return null
        return location.parentFile
    }

    /**
     * The installed countries' ISO codes, in a stable order.
     *
     * @throws TerraError.PackageNotInstalled when no package is installed at all.
     */
    @JvmStatic
    fun countries(): List<String> {
        val installed = shared().installedCountries()
        if (installed.isEmpty()) throw TerraError.PackageNotInstalled(null)
        return installed
    }

    /**
     * One country by ISO 3166-1 alpha-2 code.
     *
     * @throws TerraError.PackageNotInstalled when the package is absent — not `null`, because
     * silently returning nothing for an uninstalled country is the failure that ships a bug nobody
     * notices.
     */
    @JvmStatic
    fun country(iso2: String): Country? = Queries.country(shared().requirePackage(iso2))

    /**
     * Cities and towns whose name matches exactly, after normalization.
     *
     * Exact rather than ranked: [searchCity] is the ranked form, and having one call do "cities named
     * X" and another do "cities that look like X" is how two consumers end up with different answers
     * for the same question. Returns a list because GeoNames really does have several Antalyas.
     */
    @JvmStatic
    @JvmOverloads
    fun city(name: String, country: String? = null, limit: Int = 20): List<Place> {
        val places = mutableListOf<Place>()
        for (pkg in packagesFor(country)) {
            places += Queries.cities(pkg, name, limit)
        }
        return places
    }

    /**
     * Search every installed package.
     *
     * The three mechanisms from [`DATA_MODEL.md` §9](../../DATA_MODEL.md), in the order that makes the
     * answer total: exact, prefix, then trigram fuzzy, merged and re-sorted.
     */
    @JvmStatic
    @JvmOverloads
    fun search(query: String, country: String? = null, limit: Int = 20): List<SearchResult> =
        Queries.search(packagesFor(country), query, limit)

    /** Cities and towns matching a name, ranked. */
    @JvmStatic
    @JvmOverloads
    fun searchCity(name: String, country: String? = null, limit: Int = 20): List<Place> =
        Queries.searchCity(packagesFor(country), name, limit)

    /**
     * Streets matching a name.
     *
     * @throws TerraError.FeatureUnavailable unless a package declares the `streets` feature. An
     * empty list would look like "no streets match": a silent, plausible, wrong answer. Streets need
     * OSM, whose ODbL share-alike would constrain every downstream user, so they are outside the MVP
     * ([`ADR-004`](../docs/adr/ADR-004-geonames-first.md)).
     */
    @JvmStatic
    @JvmOverloads
    fun searchStreet(name: String, country: String? = null, limit: Int = 20): List<Place> {
        val packages = packagesFor(country)
        for (pkg in packages) pkg.requireFeature("streets")
        return emptyList()
    }

    /** A postal code, normalized and looked up. */
    @JvmStatic
    @JvmOverloads
    fun postalCode(code: String, country: String? = null): PostalCode? {
        for (pkg in packagesFor(country)) {
            Queries.postalCode(pkg, code)?.let { return it }
        }
        return null
    }

    /**
     * Every postal code covering places in a country, optionally narrowed to one city.
     *
     * Countries with no postal system return an empty list rather than an error: absence of a postal
     * system is not a failure to look one up.
     */
    @JvmStatic
    @JvmOverloads
    fun postalCodes(country: String, city: String? = null, limit: Int = QueryLimits.MAXIMUM_ENTITIES):
        List<PostalCode> = Queries.postalCodes(shared().requirePackage(country), city, limit)

    /**
     * Address suggestions for a partial query.
     *
     * Runs on every keystroke, so the cap is [QueryLimits.AUTOCOMPLETE] and is not overridable upward
     * without a reason.
     */
    @JvmStatic
    @JvmOverloads
    fun autocomplete(
        query: String,
        country: String? = null,
        language: Language? = null,
        limit: Int = QueryLimits.AUTOCOMPLETE,
    ): List<AddressSuggestion> {
        val capped = limit.coerceAtMost(QueryLimits.AUTOCOMPLETE)
        return Queries.search(packagesFor(country), query, capped).map { result ->
            val place = result.place
            // Every field but `text` is nullable: spec §36 requires the model to allow missing levels,
            // and returning a placeholder instead of null would invent data.
            val parts = mutableListOf(place.name)
            place.parentName?.let { parts += it }

            var localizedName: String? = null
            var matchedLanguage: String? = null
            if (language != null) {
                val names = Localization.placeNames(place, language)
                val best = names.firstOrNull { it.nameClass == PlaceNameClass.SHORT }
                    ?: names.firstOrNull()
                if (best != null) {
                    localizedName = best.name
                    matchedLanguage = best.language
                }
            }

            AddressSuggestion(
                text = parts.joinToString(", "),
                place = place,
                localizedName = localizedName,
                language = matchedLanguage,
                postalCode = null,
                coordinates = place.coordinates
            )
        }
    }

    /**
     * Places nearest a coordinate.
     *
     * Candidates come from `places_rtree`; the ordering is haversine in SDK code.
     */
    @JvmStatic
    @JvmOverloads
    fun nearest(coordinate: Coordinate, country: String? = null, limit: Int = 10): List<Place> =
        Queries.nearest(packagesFor(country), coordinate, limit)

    /**
     * Ready-to-render attribution notices, one per installed package.
     *
     * Spec §29: the project must never hide source attribution. A consumer under CC BY has to
     * discharge that obligation in their own UI — displaying these is the application's job, and the
     * documentation says so rather than implying the SDK handles it.
     */
    @JvmStatic
    fun attribution(): List<String> = shared().allPackages().flatMap { it.attribution }

    /** The same notices, newline-joined, ready for a settings screen. */
    @JvmStatic
    fun attributionString(): String = attribution().joinToString("\n")

    /** A country's licence identifier, e.g. `CC-BY-4.0`. */
    @JvmStatic
    fun license(iso2: String): String = shared().requirePackage(iso2).license

    /** What this build can do, for a diagnostics screen or a support request. */
    @JvmStatic
    fun capabilities(): Diagnostics {
        val resolver = shared()
        // Reported from whichever package opened first. The engine can differ per platform — the
        // JVM SDK bundles it through JDBC, Android ships whatever the device has — which is exactly
        // why this is read from the live connection instead of from a constant.
        val first = resolver.allPackages().firstOrNull()
        val capabilities = first?.database?.capabilities()
        return Diagnostics(
            sqliteVersion = first?.database?.sqliteVersion ?: "unknown",
            fts5 = capabilities?.fts5 ?: false,
            rtree = capabilities?.rtree ?: false,
            countries = resolver.installedCountries()
        )
    }

    /** The packages a query should read: one named country, or all of them. */
    internal fun packagesFor(country: String?): List<Package> {
        val resolver = shared()
        return if (country == null) resolver.allPackages() else listOf(resolver.requirePackage(country))
    }
}

/** What this SDK build can do on the machine it is running on. */
data class Diagnostics(
    val sqliteVersion: String,
    val fts5: Boolean,
    val rtree: Boolean,
    val countries: List<String>,
)