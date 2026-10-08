package com.terrakit.core

import kotlin.math.PI
import kotlin.math.asin
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.sqrt

/**
 * Core entities, `SDK_SPEC.md` §4.
 *
 * Every public type is a value type. A mutable shared cursor is never exposed, because that is the
 * obvious way to introduce a data race in a read-only library.
 */

/**
 * A latitude/longitude pair.
 *
 * `distanceTo` is haversine in kilometres. The SDK never returns an offset or a zone offset as a
 * date-time: timezones are IANA ids, resolved by the platform ([`DATA_MODEL.md` §10]).
 */
data class Coordinate(val latitude: Double, val longitude: Double) {

    /**
     * Haversine distance in kilometres.
     *
     * Spherical rather than ellipsoidal, because the alternative is an iteration count that produces
     * a visibly different number for adjacent fields and no consumer can tell which is "right". Two
     * kilometres of error on a postcode lookup is irrelevant; a third-party dependency is not.
     */
    fun distanceTo(other: Coordinate): Double {
        val earthRadiusKm = 6371.0088
        val phi1 = latitude * PI / 180
        val phi2 = other.latitude * PI / 180
        val deltaPhi = (other.latitude - latitude) * PI / 180
        val deltaLambda = (other.longitude - longitude) * PI / 180

        val a = sin(deltaPhi / 2) * sin(deltaPhi / 2) +
            cos(phi1) * cos(phi2) * sin(deltaLambda / 2) * sin(deltaLambda / 2)
        val c = 2 * atan2(sqrt(a), sqrt(1 - a))
        return earthRadiusKm * c
    }
}

/** Which side of the road traffic keeps to. */
enum class DrivingSide(val wire: String) {
    LEFT("left"),
    RIGHT("right");

    companion object {
        fun fromWire(value: String?): DrivingSide? =
            entries.firstOrNull { it.wire == value }
    }
}

/**
 * Semantic place type. Distinct from `level`, which is structural depth.
 *
 * Conflating the two is how address libraries break: Türkiye's district is at level 3, the UK's at
 * level 4, and Japan has no county anywhere ([`DATA_MODEL.md` §1]).
 */
enum class PlaceType(val wire: String) {
    COUNTRY("country"),
    REGION("region"), STATE("state"), PROVINCE("province"), PREFECTURE("prefecture"),
    CANTON("canton"), DEPARTMENT("department"),
    COUNTY("county"), DISTRICT("district"), MUNICIPALITY("municipality"), BOROUGH("borough"),
    WARD("ward"), SUBURB("suburb"),
    CITY("city"), TOWN("town"), VILLAGE("village"), NEIGHBORHOOD("neighborhood");

    /** Whether this type names a settlement, in the sense `searchCity` means. */
    val isSettlement: Boolean
        get() = this == CITY || this == TOWN || this == VILLAGE

    companion object {
        /** The types `city(_:)` and `searchCity(_:)` search, per SDK_SPEC §3. */
        val SETTLEMENT_TYPES = listOf(CITY, TOWN)

        private val byWire = entries.associateBy { it.wire }

        /**
         * Map the package's `type` column.
         *
         * Unrecognised values fall back to [REGION] rather than failing: a newer pipeline may add a
         * type this SDK predates, and refusing to read a package over an enum would defeat
         * `PACKAGE_FORMAT.md` §13's compatibility rule. The `level` is still exact, so hierarchy
         * depth is unaffected.
         */
        fun fromWire(value: String?): PlaceType = byWire[value] ?: REGION
    }
}

/** How a search result matched. Ordered: exact beats prefix beats fuzzy. */
enum class MatchKind(val wire: String, val score: Double) {
    EXACT("exact", 1.0),
    PREFIX("prefix", 0.6),
    FUZZY("fuzzy", 0.3);

    // Ordered by declaration, which Kotlin's built-in `Comparable` on enums uses. `EXACT < PREFIX <
    // FUZZY` is the ranking from DATA_MODEL.md section 9, and re-declaring `compareTo` here would be
    // both redundant and a compile error.
}

/**
 * Which flavour of name a localized name is.
 *
 * GeoNames flags both a formal and an everyday name, and they disagree: `de:Türkei` is the
 * preferred form while `de:Deutschland` is the short one. Displaying a country name must not
 * produce `Bundesrepublik Deutschland`.
 */
enum class PlaceNameClass(val wire: String) {
    PREFERRED("preferred"),
    SHORT("short"),
    EXONYM("exonym"),
    HISTORIC("historic"),
    TRANSLITERATION("transliteration"),
    COLLOQUIAL("colloquial");

    /**
     * Which name to show when several exist for the same place in the same language.
     *
     * [SHORT] first because it is the everyday form and an address UI wants that. [PREFERRED] next:
     * GeoNames' `isPreferredName` is the **formal** name, and for a language with no `short` entry it
     * is the best form the package actually holds.
     *
     * [HISTORIC] last, deliberately: `Constantinople` is a perfectly good *searchable* name and a
     * poor *display* name.
     */
    val displayRank: Int
        get() = when (this) {
            SHORT -> 0
            PREFERRED -> 1
            EXONYM -> 2
            COLLOQUIAL -> 3
            TRANSLITERATION -> 4
            HISTORIC -> 5
        }

    companion object {
        private val byWire = entries.associateBy { it.wire }

        /**
         * Map the package's `class` column, whose vocabulary differs from this enum's.
         *
         * The package says `historical`; the spec says `historic`. Mapped rather than renamed,
         * because the column value is the pipeline's contract and this enum is the SDK's.
         */
        fun fromWire(value: String?): PlaceNameClass = when (value) {
            null -> SHORT
            "historical" -> HISTORIC
            "abbreviation" -> SHORT
            else -> byWire[value] ?: COLLOQUIAL
        }
    }
}

/** A country. */
data class Country(
    val id: String,
    val iso2: String,
    val iso3: String,
    val numericCode: String,
    /** The name the package chose as the display name — `Türkiye`, `Deutschland`, `日本`. */
    val name: String,
    val continentCode: String,
    val capitalPlaceId: String?,
    val currencyCode: String,
    val phoneCode: String?,
    val internetTld: String?,
    val drivingSide: DrivingSide?,
    val timezones: List<String>,
    val languageCodes: List<String>,
    val coordinates: Coordinate?,
)

/**
 * One entry in the hierarchy.
 *
 * `type` and `level` are separate and both public. `id` is opaque: never parse it
 * ([SDK_SPEC §13](../../SDK_SPEC.md#13-anti-patterns)).
 */
data class Place(
    val id: String,
    /** Semantic: what this place *is*. */
    val type: PlaceType,
    /** Structural: depth 1…7, where 1 is the country. */
    val level: Int,
    val countryCode: String,
    val parentId: String?,
    val name: String,
    val nameNormalized: String,
    /** ISO 3166-2 where it applies, e.g. `TR-07`. */
    val administrativeCode: String?,
    val coordinates: Coordinate?,
    val population: Int?,
    /** IANA identifier, e.g. `Europe/Istanbul`. */
    val timezoneId: String?,
    val currencyCode: String?,
    /**
     * The pipeline's precomputed search rank. Higher is better.
     *
     * A constructor parameter rather than a computed property, because it *is* a property of the
     * value: it is a column in the row this came from, and reading it is how the ordering above stays
     * explainable to a caller who sees a result they did not expect first. Read, never computed —
     * ranking in SQL at query time cannot use an index, and spec §40 allows 50 ms for indexed search.
     */
    val searchRank: Long = 0,
)

/** A localized name with its script and class. */
data class PlaceName(
    val name: String,
    /** ISO 15924, when the source recorded one. */
    val script: String?,
    val language: String,
    val nameClass: PlaceNameClass,
)

/** One search hit. */
data class SearchResult(
    val place: Place,
    val matchKind: MatchKind,
    /** Which stored name matched. This is what explains a hit on `Constantinople`. */
    val matchedOn: String,
    val score: Double,
)

/**
 * One postal code and the places it covers.
 *
 * A code can cover several places — 6 in Türkiye do — and in the US a ZIP can cover thousands of
 * streets. Returning a single place would lose that ([`DATA_MODEL.md` §8](../../DATA_MODEL.md)).
 */
data class PostalCode(
    val code: String,
    val countryCode: String,
    /** The most specific place this code belongs to, when there is one. */
    val primaryPlace: Place?,
    val areas: List<Place>,
    val coordinates: Coordinate?,
) {
    val id: String get() = "$countryCode-$code"
}

/**
 * One autocomplete suggestion.
 *
 * Every field except `text` is nullable, because spec §36 requires the model to allow missing
 * levels: not every country provides every one. Returning a placeholder instead of `null` would
 * invent data.
 */
data class AddressSuggestion(
    val text: String,
    val place: Place?,
    val localizedName: String?,
    val language: String?,
    val postalCode: String?,
    val coordinates: Coordinate?,
)

/**
 * One page of results from a keyset-paginated listing.
 *
 * Keyset, never `OFFSET` ([SDK_SPEC §5](../../SDK_SPEC.md)): `OFFSET 500000` walks half a million rows
 * and throws them away, and street lists hit that immediately.
 */
data class Page<out Element>(
    val elements: List<Element>,
    /** Pass back as `after` to fetch the next page. `null` when this is the last page. */
    val nextCursor: String?,
)

/**
 * A language for localized lookups.
 *
 * Not tied to `java.util.Locale` on purpose: SDK_SPEC §3's signature is
 * `localizedName(for: Language.turkish)`, and putting the platform's locale database in the API
 * surface buys nothing.
 */
enum class Language(val tag: String) {
    ENGLISH("en"),
    TURKISH("tr"),
    GERMAN("de"),
    FRENCH("fr"),
    SPANISH("es"),
    ITALIAN("it"),
    JAPANESE("ja"),
    KOREAN("ko"),
    GEORGIAN("ka"),
    RUSSIAN("ru"),
    PORTUGUESE("pt"),
    DUTCH("nl"),
    ARABIC("ar"),
    CHINESE("zh"),
    HINDI("hi"),
    PERSIAN("fa");

    /**
     * Build from a BCP-47 tag, tolerating a region suffix.
     *
     * `tr-TR` and `tr` both resolve to TURKISH, because the fallback chain in SDK_SPEC §7 is
     * `requested language -> locale fallback -> "en" -> local name` and the caller should not have
     * to run it by hand.
     *
     * The 17 languages here are a convenience, not the set a package may hold: the Turkish package
     * carries 245 and Japan 320. Every call site that takes a [Language] has a sibling that takes a
     * raw tag.
     */
    companion object {
        private val byTag = entries.associateBy { it.tag }

        fun fromTag(tag: String): Language? {
            val cleaned = tag.replace('_', '-').lowercase()
            val base = cleaned.substringBefore('-')
            if (base.isEmpty()) return null
            byTag[base]?.let { return it }
            if (base.length >= 2) {
                byTag[base.take(2)]?.let { return it }
            }
            return null
        }
    }

    /**
     * The fallback chain for this language, most specific first.
     *
     * Ends in the local name rather than a hardcoded fallback, so a package with no `en` entry still
     * yields something sensible. Missing translations are normal, not an error.
     */
    val fallbackChain: List<String>
        get() = buildList {
            add(tag)
            if (tag.length == 2) add("$tag-001")
            if (tag != "en") add("en")
        }
}
