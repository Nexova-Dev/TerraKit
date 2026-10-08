/// Core entities, `SDK_SPEC.md` §4.
///
/// Every public type is a value type and `Sendable`. A mutable shared cursor is never exposed,
/// because that is the obvious way to introduce a data race in a read-only library.

import Foundation

// MARK: - Coordinate

/// A latitude/longitude pair.
///
/// `distance(to:)` is haversine in kilometres. The SDK never returns an offset or a zone offset
/// as a `Date`: timezones are IANA ids, resolved by the platform
/// ([`DATA_MODEL.md` §10](../../DATA_MODEL.md)).
public struct Coordinate: Hashable, Sendable, Codable {
    public let latitude: Double
    public let longitude: Double

    public init(latitude: Double, longitude: Double) {
        self.latitude = latitude
        self.longitude = longitude
    }

    /// Haversine distance in kilometres.
    ///
    /// Spherical rather than ellipsoidal, because the alternative is an iteration count that
    /// produces a visibly different number for adjacent fields and no consumer can tell which is
    /// "right". Two kilometres of error on a postcode lookup is irrelevant; a third-party
    /// dependency is not.
    public func distance(to other: Coordinate) -> Double {
        let earthRadiusKm = 6371.0088
        let φ1 = latitude * .pi / 180
        let φ2 = other.latitude * .pi / 180
        let Δφ = (other.latitude - latitude) * .pi / 180
        let Δλ = (other.longitude - longitude) * .pi / 180

        let a = sin(Δφ / 2) * sin(Δφ / 2)
            + cos(φ1) * cos(φ2) * sin(Δλ / 2) * sin(Δλ / 2)
        let c = 2 * atan2(sqrt(a), sqrt(1 - a))
        return earthRadiusKm * c
    }
}

// MARK: - Enumerations

/// Which side of the road traffic keeps to.
public enum DrivingSide: String, Sendable, Codable {
    case left
    case right
}

/// Semantic place type. Distinct from `level`, which is structural depth.
///
/// Conflating the two is how address libraries break: Türkiye's district is at level 3, the UK's
/// at level 4, and Japan has no county anywhere
/// ([`DATA_MODEL.md` §1](../../DATA_MODEL.md)).
public enum PlaceType: String, Sendable, Codable, CaseIterable {
    case country
    case region, state, province, prefecture, canton, department
    case county, district, municipality, borough, ward, suburb
    case city, town, village, neighborhood

    /// The types `city(_:)` searches, per SDK_SPEC §3.
    public static let settlementTypes: [PlaceType] = [.city, .town]

    /// Whether this type names a settlement, in the sense `searchCity` means.
    public var isSettlement: Bool {
        switch self {
        case .city, .town, .village: true
        default: false
        }
    }
}

/// How a search result matched.
public enum MatchKind: Int, Comparable, Sendable, Codable {
    case exact
    case prefix
    case fuzzy

    public static func < (lhs: MatchKind, rhs: MatchKind) -> Bool {
        lhs.rawValue < rhs.rawValue
    }

    /// A comparable score, so a caller can blend kinds with other signals if it wants to.
    public var score: Double {
        switch self {
        case .exact: 1.0
        case .prefix: 0.6
        case .fuzzy: 0.3
        }
    }
}

/// Which flavour of name a localized name is.
///
/// GeoNames flags both a formal and an everyday name, and they disagree: `de:Türkei` is the
/// preferred form while `de:Deutschland` is the short one. Displaying `country.name` must not
/// produce `Bundesrepublik Deutschland`.
public enum PlaceNameClass: String, Sendable, Codable {
    case preferred        // formal: 'Republic of Türkiye'
    case short            // everyday: 'Türkiye'
    case exonym           // name used by speakers of another language
    case historic         // 'Constantinople'
    case transliteration  // romanization of a non-Latin name
    case colloquial

    /// Map the package's `class` column, whose vocabulary differs from this enum's.
    ///
    /// The package says `historical`; the spec says `historic`. Mapped rather than renamed, because
    /// the column value is the pipeline's contract and this enum is the SDK's.
    public init(packageClass: String?) {
        switch packageClass {
        case "historical": self = .historic
        case "exonym": self = .exonym
        case "colloquial": self = .colloquial
        case "transliteration": self = .transliteration
        case "abbreviation": self = .short
        case "short": self = .short
        case "preferred": self = .preferred
        case .some(let other): self = PlaceNameClass(rawValue: other) ?? .colloquial
        case nil: self = .short
        }
    }

    /// Which name to show when several exist for the same place in the same language.
    ///
    /// `short` first because it is the everyday form and an address UI wants that. `preferred`
    /// next: GeoNames' `isPreferredName` is the **formal** name, and for a language with no `short`
    /// entry it is the best form the package actually holds — see
    /// `ConformanceTests.testTheEnglishFormalNameIsAPackageLimitation`.
    ///
    /// `historic` last, deliberately: `Constantinople` is a perfectly good *searchable* name and a
    /// poor *display* name.
    public var displayRank: Int {
        switch self {
        case .short: 0
        case .preferred: 1
        case .exonym: 2
        case .colloquial: 3
        case .transliteration: 4
        case .historic: 5
        }
    }
}

// MARK: - Entities

/// A country.
public struct Country: Identifiable, Hashable, Sendable, Codable {
    public let id: String
    public let iso2: String
    public let iso3: String
    public let numericCode: String
    /// The name the package chose as the display name — `Türkiye`, `Deutschland`, `日本`.
    public let name: String
    public let continentCode: String
    public let capitalPlaceID: String?
    public let currencyCode: String
    public let phoneCode: String?
    public let internetTLD: String?
    public let drivingSide: DrivingSide?
    public let timezones: [String]
    public let languageCodes: [String]
    public let coordinates: Coordinate?
}

/// One entry in the hierarchy.
///
/// `type` and `level` are separate and both public. `id` is opaque: never parse it
/// ([SDK_SPEC §13](../../SDK_SPEC.md#13-anti-patterns)).
public struct Place: Identifiable, Hashable, Sendable, Codable {
    public let id: String
    /// Semantic: what this place *is*.
    public let type: PlaceType
    /// Structural: depth 1…7, where 1 is the country.
    public let level: Int
    public let countryCode: String
    public let parentID: String?
    public let name: String
    public let nameNormalized: String
    /// ISO 3166-2 where it applies, e.g. `TR-07`.
    public let administrativeCode: String?
    public let coordinates: Coordinate?
    public let population: Int?
    /// IANA identifier, e.g. `Europe/Istanbul`.
    public let timezoneID: String?
    public let currencyCode: String?

    /// The pipeline's precomputed search rank.
    ///
    /// Internal, and deliberately so: `SDK_SPEC.md` §4.2 does not list it, and exposing it would
    /// invite a consumer to rank by it as though the SDK's ordering were negotiable. Read here,
    /// never computed — ranking in SQL at query time cannot use an index, and spec §40 allows
    /// 50 ms for indexed search.
    let searchRank: Int64
}

/// A localized name with its script and class.
public struct PlaceName: Hashable, Sendable {
    public let name: String
    /// ISO 15924, when the source recorded one.
    public let script: String?
    public let language: String
    public let nameClass: PlaceNameClass

    public init(name: String, script: String?, language: String, nameClass: PlaceNameClass) {
        self.name = name
        self.script = script
        self.language = language
        self.nameClass = nameClass
    }
}

/// One search hit.
public struct SearchResult: Sendable, Hashable {
    public let place: Place
    public let matchKind: MatchKind
    /// Which stored name matched. This is what explains a hit on `Constantinople`, and its absence
    /// is what made such results mysterious.
    public let matchedOn: String
    public let score: Double

    public init(place: Place, matchKind: MatchKind, matchedOn: String, score: Double) {
        self.place = place
        self.matchKind = matchKind
        self.matchedOn = matchedOn
        self.score = score
    }
}

/// One postal code and the places it covers.
public struct PostalCode: Identifiable, Hashable, Sendable {
    public let code: String
    public let countryCode: String
    /// The most specific place this code belongs to, when there is one.
    public let primaryPlace: Place?
    /// Every place it covers. A code can cover several — 6 in Türkiye do — and in the US a ZIP can
    /// cover thousands of streets. Returning a single place would lose that
    /// ([`DATA_MODEL.md` §8](../../DATA_MODEL.md)).
    public let areas: [Place]
    public let coordinates: Coordinate?

    public var id: String { "\(countryCode)-\(code)" }
}

/// One autocomplete suggestion.
///
/// Every field except `text` is optional, because spec §36 requires the model to allow missing
/// levels: not every country provides every one. Returning a placeholder instead of `nil` would
/// invent data.
public struct AddressSuggestion: Sendable, Hashable {
    public let text: String
    public let place: Place?
    public let localizedName: String?
    public let language: String?
    public let postalCode: String?
    public let coordinates: Coordinate?

    public init(
        text: String,
        place: Place?,
        localizedName: String?,
        language: String?,
        postalCode: String?,
        coordinates: Coordinate?
    ) {
        self.text = text
        self.place = place
        self.localizedName = localizedName
        self.language = language
        self.postalCode = postalCode
        self.coordinates = coordinates
    }
}

/// One page of results from a keyset-paginated listing.
///
/// Keyset, never `OFFSET` ([SDK_SPEC §5](../../SDK_SPEC.md)): `OFFSET 500000` walks half a million
/// rows and throws them away, and street lists hit that immediately.
public struct Page<Element: Sendable>: Sendable {
    public let elements: [Element]
    /// Pass back as `after:` to fetch the next page. `nil` when this is the last page.
    public let nextCursor: String?

    public init(elements: [Element], nextCursor: String?) {
        self.elements = elements
        self.nextCursor = nextCursor
    }
}

/// A language for localized lookups.
///
/// `Locale`-free on purpose: SDK_SPEC §3's signature is `localizedName(for: .turkish)`, and
/// tying the enum to `Locale` would put Foundation's locale database in the API surface for no
/// gain. `Locale.Language` is accepted at the call site.
public enum Language: String, Sendable, Codable, CaseIterable {
    case english = "en"
    case turkish = "tr"
    case german = "de"
    case french = "fr"
    case spanish = "es"
    case italian = "it"
    case japanese = "ja"
    case korean = "ko"
    case georgian = "ka"
    case russian = "ru"
    case portuguese = "pt"
    case dutch = "nl"
    case arabic = "ar"
    case chinese = "zh"
    case hindi = "hi"
    case persian = "fa"

    /// Build from a BCP-47 tag, tolerating a region suffix.
    ///
    /// `tr-TR` and `tr` both resolve to `.turkish`, because the fallback chain in SDK_SPEC §7 is
    /// `requested language -> locale fallback (tr-TR -> tr -> default) -> "en" -> local name` and
    /// the caller should not have to run it by hand.
    public init?(bcp47 tag: String) {
        // `tr-TR`, `tr_TR`, `TR`, `tr` all reach `.turkish`. The package carries 245 languages for
        // Türkiye and 320 for Japan, so the enum is a convenience for the common ones and every
        // call site also accepts a raw tag; this is not a closed set of what a package may hold.
        let cleaned = tag
            .replacingOccurrences(of: "_", with: "-")
            .lowercased()
        let base = cleaned.split(separator: "-").first.map(String.init) ?? ""
        guard !base.isEmpty else { return nil }

        if let exact = Language(rawValue: base) {
            self = exact
            return
        }
        // A three-letter tag: match on its two-letter prefix when the package's language is that.
        if base.count >= 2 {
            let prefix = String(base.prefix(2))
            if let match = Language.allCases.first(where: { $0.rawValue == prefix }) {
                self = match
                return
            }
        }
        return nil
    }

    /// The fallback chain for a requested language, most specific first.
    ///
    /// Ends in the local name rather than a hardcoded fallback, so a package with no `en` entry
    /// still yields something sensible. Missing translations are normal, not an error.
    public var fallbackChain: [String] {
        var chain = [rawValue]
        if rawValue.count == 2 {
            chain.append("\(rawValue)-001")
        }
        if chain.first != "en" {
            chain.append("en")
        }
        return chain
    }
}