package com.terrakit.core

import java.io.File

/**
 * One installed package: its database plus what its manifest says about it.
 */
class Package internal constructor(
    /**
     * The package database.
     *
     * Internal: a consumer has no business running its own statements against a package file, and
     * SDK_SPEC §1 keeps the public surface to what the query engine offers. Everything a caller might
     * want about the package is on this class or on [Terra].
     */
    internal val database: Database,
    val countryCode: String,
    val directory: File,
    /** `schemaVersion` from the manifest. */
    val schemaVersion: Int,
    /** The dataset's level: `basic`, `administrative`, `cities`, `postal`, `full`. */
    val level: String,
    /** The dataset version, e.g. `2026.10.1+9920`. */
    val version: String,
    /**
     * Declared features, e.g. `["autocomplete", "streets"]`.
     *
     * Checked rather than inferred from a table existing: `searchStreet` on a `cities` package cannot
     * return a useful answer, and an empty list would look like "no streets match".
     */
    val features: Set<String>,
    /** Licence, e.g. `CC-BY-4.0`. */
    val license: String,
    /** Ready-to-render attribution notices. */
    val attribution: List<String>,
) {
    /**
     * Whether this package provides a declared feature.
     *
     * @throws TerraError.FeatureUnavailable when it does not.
     */
    fun requireFeature(feature: String) {
        if (!features.contains(feature)) {
            throw TerraError.FeatureUnavailable(feature, level)
        }
    }

    companion object {
        /** The schema version this SDK supports. */
        const val SUPPORTED_SCHEMA_VERSION = 1
    }
}

/**
 * Finds packages and holds the resolved set.
 *
 * Resolution order, first match wins, resolved once and cached:
 *
 * ```text
 * 1. explicit configuration        Terra.configure(packages = ...)
 * 2. the application bundle        iOS/macOS resource, Android assets
 * 3. .terrakit/packages/...        local development
 * ```
 *
 * Step 2 is what makes `import TerraKit` work in a shipped app, since an app reads only from its
 * bundle. On Android the assets must be copied out of the APK before SQLite can open them; see
 * `AssetInstaller`.
 *
 * A package that is absent produces `TerraError.PackageNotInstalled`, not an empty result. Silently
 * returning nothing for an uninstalled country is the failure mode that ships a bug nobody notices
 * until a customer reports an empty dropdown.
 */
class PackageResolver(val roots: List<File>) {

    private val lock = Any()
    private val cache = mutableMapOf<String, Package?>()

    /**
     * Key ids the caller trusts, for signature checking by a consumer that wants it.
     *
     * The SDK does not verify signatures itself — `terrakit add` does, before the package is ever
     * written to disk ([`SECURITY.md` §3](../../SECURITY.md)). This exists so an app that bundles
     * packages itself can check the same keyring rather than trusting the file's presence.
     */
    var trustedKeyIds: Set<String> = emptySet()

    /**
     * One candidate package, with the country and version it declares.
     *
     * Two layouts are accepted, because both exist in the wild:
     *
     * ```text
     * canonical   <root>/TR/2026.10.1-9920/data.sqlite      <- .terrakit/packages
     * flat        <root>/terrakit-data-tr-2026.10.1+9920/  <- a dataset directory
     * ```
     *
     * The canonical one is what `terrakit add` writes and what a shipped app's bundle contains. The
     * flat one is what a person has after building a dataset into a directory, and pointing the SDK
     * at that directory is a reasonable thing to want.
     */
    private data class Candidate(val country: String, val version: String, val directory: File)

    private fun candidates(): List<Candidate> {
        val found = mutableListOf<Candidate>()
        for (root in roots) {
            val entries = root.listFiles() ?: continue
            for (entry in entries.sortedBy { it.name }) {
                if (!entry.isDirectory) continue

                // Flat: terrakit-data-<cc>-<version>
                if (entry.name.startsWith("terrakit-data-")) {
                    val parts = entry.name.split("-", limit = 4)
                    if (parts.size != 4 || parts[0] != "terrakit" || parts[1] != "data") continue
                    val country = parts[2].uppercase()
                    if (country.length != 2 || !country.all(Char::isLetter)) continue
                    if (!File(entry, "data.sqlite").isFile) continue
                    found += Candidate(country, parts[3], entry)
                    continue
                }

                // Canonical: <CC>/<version>
                if (entry.name.length != 2) continue
                if (entry.name != entry.name.uppercase()) continue
                if (!entry.name.all(Char::isLetter)) continue
                val versions = entry.listFiles() ?: continue
                for (version in versions.sortedBy { it.name }) {
                    if (version.isDirectory && File(version, "data.sqlite").isFile) {
                        found += Candidate(entry.name, version.name, version)
                    }
                }
            }
        }
        return found
    }

    /**
     * Every installed country, in a stable order.
     *
     * Sorted by ISO code so the result is deterministic: an unsorted directory listing makes a
     * conformance test compare two different orders and report a spurious difference.
     */
    fun installedCountries(): List<String> = candidates().map { it.country }.distinct().sorted()

    /**
     * The package for a country, or `null` when it is absent.
     *
     * Resolves the **newest** version when several are present. `terrakit update` removes the version
     * it supersedes, so more than one means a rollback happened and the newest is the one a user
     * means.
     *
     * Ordering is by the version each manifest declares, not by directory name: directory names are
     * the version with `+` replaced by `-`, and string ordering puts `2026.10.10` before
     * `2026.10.9`, so "newest" would be whichever one a lexical sort happens to put last.
     */
    fun packageFor(countryCode: String): Package? {
        val code = countryCode.uppercase()
        synchronized(lock) { cache[code] }?.let { return it }

        val matching = candidates()
            .filter { it.country == code }
            .sortedWith { lhs, rhs -> compareVersions(versionKeyOf(lhs.directory), versionKeyOf(rhs.directory)) }

        for (candidate in matching.asReversed()) {
            val manifestFile = File(candidate.directory, "manifest.json")
            val manifest = try {
                Json.parse(manifestFile.readText()).jsonObject()
            } catch (error: Throwable) {
                null
            } ?: continue

            val opened = try {
                openPackage(manifest, candidate, code)
            } catch (error: TerraError) {
                // A damaged or unreadable package is skipped in favour of another version. One bad
                // package must not make every other country unqueryable.
                continue
            }
            synchronized(lock) { cache[code] = opened }
            return opened
        }
        return null
    }

    /** The package for a country, or [TerraError.PackageNotInstalled]. */
    fun requirePackage(countryCode: String?): Package {
        val pkg = countryCode?.let { packageFor(it) }
            ?: throw TerraError.PackageNotInstalled(countryCode?.uppercase())
        return pkg
    }

    /** Every opened package, in country order. */
    fun allPackages(): List<Package> = installedCountries().mapNotNull { packageFor(it) }

    private fun openPackage(manifest: Map<String, Any?>, candidate: Candidate, code: String): Package {
        val schemaVersion = manifest.int("schemaVersion")
            ?: throw TerraError.PackageCorrupted(checksumMismatch = false)
        val minReader = manifest.int("minReaderVersion")
        if (minReader != null && minReader > Terra.SDK_VERSION_NUMBER) {
            throw TerraError.SchemaTooNew(schemaVersion, Package.SUPPORTED_SCHEMA_VERSION, Terra.SDK_VERSION)
        }

        val database = Database(
            File(candidate.directory, "data.sqlite").path,
            code
        )
        return Package(
            database = database,
            countryCode = code,
            directory = candidate.directory,
            schemaVersion = schemaVersion,
            level = manifest.string("level") ?: "unknown",
            version = manifest.string("version") ?: "unknown",
            features = manifest.stringList("features").toSet(),
            license = manifest.string("license") ?: "unknown",
            attribution = manifest.stringList("attribution")
        )
    }

    companion object {
        /**
         * A comparable key for a package directory's declared version.
         *
         * Numeric, so `2026.10.10` beats `2026.10.9`. The `+suffix` is ignored, following semver:
         * `2026.10.1+a6c3` and `2026.10.1+e83e` are the same release line.
         */
        fun versionKeyOf(directory: File): List<Int> {
            val manifest = try {
                Json.parse(File(directory, "manifest.json").readText()).jsonObject()
            } catch (error: Throwable) {
                null
            } ?: return listOf(0)
            val version = manifest.string("version") ?: return listOf(0)
            return version.substringBefore('+').split('.').map { it.toIntOrNull() ?: 0 }
        }

        /** Element-wise and padded, so `[2026, 10]` does not sort above `[2026, 10, 1]`. */
        internal fun compareVersions(a: List<Int>, b: List<Int>): Int {
            for (i in 0 until maxOf(a.size, b.size)) {
                val left = if (i < a.size) a[i] else 0
                val right = if (i < b.size) b[i] else 0
                if (left != right) return left.compareTo(right)
            }
            return 0
        }
    }
}