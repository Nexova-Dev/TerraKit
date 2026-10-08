/// The `Terra` namespace — the entry points spec §50 and §51 name.
///
/// Each is a thin wrapper over one indexed query, so the query plans in `SDK_SPEC.md` §3 are the
/// contract rather than an implementation detail.
///
/// **Offline by construction.** No query path contains a network branch
/// ([`ARCHITECTURE.md` §1](../../ARCHITECTURE.md)). The SDK does not download: `Terra.country("JP")`
/// on a machine with no JP package throws `.packageNotInstalled`. Downloading is the CLI's job.

import Foundation

/// The TerraKit SDK.
public enum Terra {

    /// This SDK's version, reported in `schemaTooNew` so the error names what is needed.
    public static let sdkVersion = "1.0.0"

    /// `sdkVersion` as a comparable number, for `minReaderVersion` checks.
    public static let sdkVersionNumber = 1_000_000

    // MARK: - Configuration

    private static let lock = NSLock()
    nonisolated(unsafe) private static var resolver: PackageResolver?

    /// Use an explicit package directory instead of discovery.
    ///
    /// For a test fixture or an unusual layout. The directory must contain one subdirectory per
    /// country, each holding a version directory with `data.sqlite` and `manifest.json`.
    public static func configure(packageDirectory url: URL, trustedKeyIDs: Set<String> = []) throws {
        try configure(roots: [url], trustedKeyIDs: trustedKeyIDs)
    }

    /// Use explicit package roots, in priority order.
    public static func configure(roots: [URL], trustedKeyIDs: Set<String> = []) throws {
        guard !roots.isEmpty else {
            throw TerraError.packageNotInstalled(country: nil)
        }
        let newResolver = PackageResolver(roots: roots)
        newResolver.trustedKeyIDs = trustedKeyIDs
        lock.lock()
        resolver = newResolver
        lock.unlock()
    }

    /// Open packages by discovery: the application bundle first, then `.terrakit/packages/`.
    ///
    /// Bundle first because that is what a shipped app has. Resolution is one `stat` per country,
    /// done once and cached.
    public static func open(bundle: Bundle = .main, workingDirectory: URL = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)) throws -> PackageResolver {
        let roots = PackageResolver.defaultRoots(bundle: bundle, workingDirectory: workingDirectory)
        let discovered = PackageResolver(roots: roots)
        lock.lock()
        resolver = discovered
        lock.unlock()
        return discovered
    }

    /// The active resolver, opening by discovery if `open()` has not been called.
    ///
    /// Auto-opening is what makes `Terra.countries` work in a script with no setup, which is the
    /// common case. It is cached, so the discovery walk happens once.
    public static func shared() throws -> PackageResolver {
        lock.lock()
        let existing = resolver
        lock.unlock()
        if let existing { return existing }
        return try open()
    }

    /// The installed countries' ISO codes, in a stable order.
    public static func countries() throws -> [String] {
        try shared().installedCountries()
    }

    /// One country by ISO 3166-1 alpha-2 code.
    ///
    /// - Throws: `.packageNotInstalled` when the package is absent. Not `nil` — silently returning
    ///   nothing for an uninstalled country is the failure that ships a bug nobody notices.
    public static func country(_ iso2: String) throws -> Country? {
        let package = try shared().requirePackage(iso2)
        return try Queries.country(from: package)
    }

    /// Cities and towns whose name matches exactly, after normalization.
    ///
    /// Exact rather than ranked: `searchCity(_:)` is the ranked form, and having one call do
    /// "cities named X" and another do "cities that look like X" is how two consumers end up with
    /// different answers for the same question. Returns an array because GeoNames really does have
    /// several Antalyas.
    ///
    /// `country:` narrows the search the same way every other entry point does. It was missing here
    /// and is not an oversight to preserve: `SDK_SPEC.md` §1 requires both SDKs to expose the same
    /// API in the same shape, and one entry point out of eight with a different signature is
    /// exactly the divergence that rule exists to prevent.
    public static func city(_ name: String, country: String? = nil, limit: Int = 20) throws -> [Place] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        var places: [Place] = []
        for package in packages {
            places += try Queries.cities(in: package, name: name, limit: limit)
        }
        return places
    }

    // MARK: - Search

    /// Search every installed package.
    ///
    /// The three mechanisms from [`DATA_MODEL.md` §9](../../DATA_MODEL.md), in the order that makes
    /// the answer total: exact, prefix, then trigram fuzzy, merged and re-sorted.
    public static func search(
        _ query: String,
        country: String? = nil,
        limit: Int = 20
    ) throws -> [SearchResult] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        return try Queries.search(packages, query, limit: limit)
    }

    /// Cities and towns matching a name, ranked.
    public static func searchCity(_ name: String, country: String? = nil, limit: Int = 20) throws -> [Place] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        return try Queries.searchCity(packages, name, limit: limit)
    }

    /// Streets matching a name.
    ///
    /// - Throws: `.featureUnavailable` unless a package declares the `streets` feature. Returning
    ///   an empty array would look like "no streets match": a silent, plausible, wrong answer.
    ///   Streets need OSM, whose ODbL share-alike would constrain every downstream user, so they
    ///   are outside the MVP ([`ADR-004`](../../docs/adr/ADR-004-geonames-first.md)).
    public static func searchStreet(_ name: String, country: String? = nil, limit: Int = 20) throws -> [Place] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        for package in packages {
            try package.hasFeature("streets")
        }
        return []
    }

    /// A postal code, normalized and looked up.
    public static func postalCode(_ code: String, country: String? = nil) throws -> PostalCode? {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        for package in packages {
            if let hit = try Queries.postalCode(in: package, code: code) {
                return hit
            }
        }
        return nil
    }

    /// Every postal code covering places in a country, optionally narrowed to one city.
    ///
    /// Countries with no postal system return an empty array rather than an error: absence of a
    /// postal system is not a failure to look one up.
    public static func postalCodes(country: String, city: String? = nil, limit: Int = QueryLimits.maximumEntities) throws -> [PostalCode] {
        let package = try shared().requirePackage(country)
        return try Queries.postalCodes(in: package, city: city, limit: limit)
    }

    /// Address suggestions for a partial query.
    ///
    /// Runs on every keystroke, so the default cap is 50 and is not overridable upward without a
    /// reason.
    public static func autocomplete(
        _ query: String,
        country: String? = nil,
        language: Language? = nil,
        limit: Int = QueryLimits.autocomplete
    ) throws -> [AddressSuggestion] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        return try Queries.autocomplete(packages, query, language: language, limit: limit)
    }

    /// Places nearest a coordinate.
    ///
    /// Candidates come from `places_rtree`; the ordering is haversine in SDK code. The R\*Tree is a
    /// candidate generator, never the answer ([`ADR-005`](../../docs/adr/ADR-005-postgis-sqlite-rtree.md)):
    /// a bounding-box prefilter degrades at high latitudes, exactly where data is densest.
    public static func nearest(
        to coordinate: Coordinate,
        country: String? = nil,
        limit: Int = 10
    ) throws -> [Place] {
        let resolver = try shared()
        let packages = try country.map { [try resolver.requirePackage($0)] } ?? (try resolver.allPackages())
        return try Queries.nearest(packages, to: coordinate, limit: limit)
    }

    // MARK: - Attribution

    /// Ready-to-render attribution notices, one per installed package.
    ///
    /// Spec §29: the project must never hide source attribution. A consumer under CC BY has to
    /// discharge that obligation in their own UI — displaying these is the application's job, and
    /// the documentation says so rather than implying the SDK handles it.
    public static func attribution() throws -> [String] {
        try shared().allPackages().flatMap(\.attribution)
    }

    /// The same notices, newline-joined, ready for a settings screen.
    public static func attributionString() throws -> String {
        try attribution().joined(separator: "\n")
    }

    /// A country's licence identifier, e.g. `CC-BY-4.0`.
    public static func license(for iso2: String) throws -> String? {
        try shared().requirePackage(iso2).license
    }

    // MARK: - Diagnostics

    /// What this build can do, for a diagnostics screen or a support request.
    public static func capabilities() throws -> (sqliteVersion: String, fts5: Bool, rtree: Bool, countries: [String]) {
        let resolver = try shared()
        let version = try Database.sqliteVersion()
        var fts5 = false
        var rtree = false
        if let first = try resolver.allPackages().first {
            (fts5, rtree) = first.database.hasSearchIndexes()
        }
        return (version, fts5, rtree, resolver.installedCountries())
    }
}

// MARK: - Row mapping
//
// One place where a row becomes a value. Pinned columns rather than `SELECT *`
// ([SDK_SPEC §13](../../SDK_SPEC.md#13-anti-patterns)): unknown columns must not become unknown SDK
// properties, so a package built with a newer schema still reads cleanly.
enum Queries {

    /// Columns every place projection reads, in a fixed order.
    static let placeColumns = """
        p.id, p.name, p.type, p.level, p.country_iso2, p.parent_id, p.name_norm, \
        p.admin_code, p.latitude, p.longitude, p.population, p.timezone_id, p.currency_code, \
        p.search_rank
        """

    static func place(from row: Database.Statement) throws -> Place {
        let latitude = row.double(8)
        let longitude = row.double(9)
        return Place(
            id: row.string(0) ?? "",
            type: PlaceType(rawValue: row.string(2) ?? "") ?? .region,
            level: Int(row.int(3) ?? 0),
            countryCode: row.string(4) ?? "",
            parentID: row.string(5),
            name: row.string(1) ?? "",
            nameNormalized: row.string(6) ?? "",
            administrativeCode: row.string(7),
            coordinates: (latitude != nil && longitude != nil)
                ? Coordinate(latitude: latitude!, longitude: longitude!)
                : nil,
            population: row.int(10).map(Int.init),
            timezoneID: row.string(11),
            currencyCode: row.string(12),
            searchRank: row.int(13) ?? 0
        )
    }

    static func country(from package: Package) throws -> Country? {
        try package.database.first(
            """
            SELECT id, iso2, iso3, numeric_code, name, continent_code, capital_place_id,
                   currency_code, phone_code, internet_tld, driving_side, latitude, longitude
            FROM countries
            """
        ) { row in
            let latitude = row.double(11)
            let longitude = row.double(12)
            let value = Country(
                id: row.string(0) ?? "",
                iso2: row.string(1) ?? "",
                iso3: row.string(2) ?? "",
                numericCode: row.string(3) ?? "",
                name: row.string(4) ?? "",
                continentCode: row.string(5) ?? "",
                capitalPlaceID: row.string(6),
                currencyCode: row.string(7) ?? "",
                phoneCode: row.string(8),
                internetTLD: row.string(9),
                drivingSide: row.string(10).flatMap { DrivingSide(rawValue: $0) },
                timezones: [],
                languageCodes: [],
                coordinates: nil
            )

            // Timezones and languages are reference tables, and a country may have neither when
            // the pipeline could not populate them. Populated best-effort rather than failing:
            // a country row with no timezone list is still a useful answer.
            let timezones = try package.database.query(
                """
                SELECT tz.id FROM country_timezones c JOIN timezones tz ON tz.id = c.timezone_id
                WHERE c.country_iso2 = ?1 ORDER BY tz.id
                """,
                bindings: [.text(value.iso2)],
                limit: 64,
                map: { try $0.string(0) ?? "" }
            )
            let languages = try package.database.query(
                "SELECT lang FROM country_languages WHERE country_iso2 = ?1 ORDER BY lang",
                bindings: [.text(value.iso2)],
                limit: 512,
                map: { try $0.string(0) ?? "" }
            )

            return Country(
                id: value.id, iso2: value.iso2, iso3: value.iso3, numericCode: value.numericCode,
                name: value.name, continentCode: value.continentCode,
                capitalPlaceID: value.capitalPlaceID, currencyCode: value.currencyCode,
                phoneCode: value.phoneCode, internetTLD: value.internetTLD,
                drivingSide: value.drivingSide,
                timezones: timezones.isEmpty ? [] : timezones,
                languageCodes: languages,
                coordinates: (latitude != nil && longitude != nil)
                    ? Coordinate(latitude: latitude!, longitude: longitude!)
                    : nil
            )
        }
    }

    static func cities(in package: Package, name: String, limit: Int) throws -> [Place] {
        let normalized = Normalization.normalize(name)
        guard !normalized.isEmpty else { return [] }

        // The type list is bound, not interpolated.
        //
        // It was built as `IN (city,town)` — without quotes — which SQLite reads as two column
        // references rather than two strings, and every call failed with "no such column: city".
        // The conformance battery missed it because `searchCity` is a separate query that spells its
        // literals correctly, so this entry point had no coverage at all. Binding removes the whole
        // class: there is no longer a string to forget to quote.
        let types = PlaceType.settlementTypes
        let placeholders = (0..<types.count).map { "?\(2 + $0)" }.joined(separator: ",")
        let limitSlot = 2 + types.count

        return try package.database.query(
            """
            SELECT \(placeColumns) FROM places p
            WHERE p.name_norm = ?1 AND p.type IN (\(placeholders))
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?\(limitSlot)
            """,
            bindings: [.text(normalized)]
                + types.map { .text($0.rawValue) }
                + [.integer(Int64(min(limit, QueryLimits.maximumEntities)))],
            map: place
        )
    }

    /// The three-mechanism search, merged and ranked.
    static func search(_ packages: [Package], _ query: String, limit: Int) throws -> [SearchResult] {
        let normalized = Normalization.normalize(query)
        guard !normalized.isEmpty else { return [] }

        var results: [SearchResult] = []
        var seen = Set<String>()

        for package in packages {
            // 1. exact
            for place in try exact(in: package, normalized) {
                append(place, kind: .exact, matchedOn: place.nameNormalized, into: &results, seen: &seen)
            }
            // 2. prefix — escapes LIKE wildcards so a user's `%` is a literal per cent sign
            //    rather than a full scan.
            if results.count < limit {
                for place in try prefixed(in: package, normalized) {
                    append(place, kind: .prefix, matchedOn: place.nameNormalized, into: &results, seen: &seen)
                }
            }
            // 3. trigram, only for queries the tokenizer can index. A one- or two-character query
            //    cannot produce a trigram, and asking anyway would be a scan.
            //
            //    Documented behaviour, not a hidden fallback, and far more visible in CJK:
            //    `search("東京都")` works while `search("東京")` does not, because Japanese place
            //    names are routinely two characters (DATA_MODEL.md section 9).
            if results.count < limit && normalized.count >= Normalization.searchKeyMinimumLength {
                for (place, key) in try fuzzy(in: package, normalized) {
                    append(place, kind: .fuzzy, matchedOn: key, into: &results, seen: &seen)
                }
            }
        }

        // The total order from DATA_MODEL.md section 9. `id` last: it makes the order total, which
        // is what lets the conformance suite compare result SETS rather than counts.
        results.sort { lhs, rhs in
            if lhs.matchKind != rhs.matchKind { return lhs.matchKind < rhs.matchKind }
            if lhs.place.searchRank != rhs.place.searchRank {
                return lhs.place.searchRank > rhs.place.searchRank
            }
            if lhs.place.id != rhs.place.id { return lhs.place.id < rhs.place.id }
            return lhs.matchedOn < rhs.matchedOn
        }

        // One row per place. Searching "Antalya" against the Turkish package matches the province
        // through five indexed names -- its own, "Ili", the Basque "probintzia" and two Georgian
        // transliterations -- and the CLI learned this the hard way. A search result is a place,
        // not a name variant.
        var unique: [SearchResult] = []
        var kept = Set<String>()
        for result in results where kept.insert(result.place.id).inserted {
            unique.append(result)
            if unique.count >= limit { break }
        }
        return unique
    }

    static func searchCity(_ packages: [Package], _ name: String, limit: Int) throws -> [Place] {
        var places: [Place] = []
        for package in packages {
            let normalized = Normalization.normalize(name)
            guard !normalized.isEmpty else { continue }
            places += try package.database.query(
                """
                SELECT \(placeColumns) FROM places p
                WHERE p.name_norm = ?1 AND p.type IN ('city','town')
                ORDER BY p.search_rank DESC, p.id ASC
                LIMIT ?2
                """,
                bindings: [.text(normalized), .integer(Int64(min(limit, QueryLimits.maximumEntities)))],
                map: place
            )
        }
        return places
    }

    private static func exact(in package: Package, _ key: String) throws -> [Place] {
        try package.database.query(
            """
            SELECT \(placeColumns) FROM places p
            WHERE p.name_norm = ?1
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            bindings: [.text(key), .integer(Int64(QueryLimits.maximumEntities))],
            map: place
        )
    }

    private static func prefixed(in package: Package, _ key: String) throws -> [Place] {
        try package.database.query(
            """
            SELECT \(placeColumns) FROM places p
            WHERE p.name_norm LIKE ?1 ESCAPE '\\'
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            bindings: [
                .text(escapeLike(key) + "%"),
                .integer(Int64(QueryLimits.maximumEntities)),
            ],
            map: place
        )
    }

    private static func fuzzy(in package: Package, _ key: String) throws -> [(Place, String)] {
        // Quoted, because an unquoted term is FTS5 query syntax: a user typing `OR` or `*` would
        // change the query rather than search for a word.
        guard package.database.hasSearchIndexes().fts5 else { return [] }
        return try package.database.query(
            """
            SELECT \(placeColumns), f.name_norm
            FROM search_fts f JOIN places p ON p.id = f.place_id
            WHERE search_fts MATCH ?1
            ORDER BY p.search_rank DESC, p.id ASC
            LIMIT ?2
            """,
            bindings: [
                .text("\"" + key.replacingOccurrences(of: "\"", with: "\"\"") + "\""),
                .integer(Int64(QueryLimits.maximumEntities)),
            ],
            map: { row in
                let place = try Queries.place(from: row)
                return (place, row.string(14) ?? place.nameNormalized)
            }
        )
    }

    private static func append(
        _ place: Place,
        kind: MatchKind,
        matchedOn: String,
        into results: inout [SearchResult],
        seen: inout Set<String>
    ) {
        let key = "\(place.id)|\(matchedOn)"
        guard seen.insert(key).inserted else { return }
        results.append(
            SearchResult(place: place, matchKind: kind, matchedOn: matchedOn, score: kind.score)
        )
    }

    /// Escape LIKE wildcards so a user's `%` or `_` is literal.
    ///
    /// Without this, typing `Ant_ya` returns every six-letter place starting with "Ant" and the
    /// user has no way to know why.
    static func escapeLike(_ input: String) -> String {
        input
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "%", with: "\\%")
            .replacingOccurrences(of: "_", with: "\\_")
    }

    // MARK: - Postal

    static func postalCode(in package: Package, code: String) throws -> PostalCode? {
        let normalized = Normalization.postalNormalize(code)
        guard !normalized.isEmpty else { return nil }

        return try package.database.first(
            """
            SELECT code, latitude, longitude FROM postal_codes
            WHERE country_iso2 = ?1 AND code_norm = ?2
            """,
            bindings: [.text(package.countryCode), .text(normalized)]
        ) { row in
            let latitude = row.double(1)
            let longitude = row.double(2)

            // A code can cover several places — 6 in Türkiye span more than one district — so
            // `areas` is a list and the primary is the most specific one (DATA_MODEL.md section 8).
            let areas = try package.database.query(
                """
                SELECT \(placeColumns), a.is_primary
                FROM postal_areas a JOIN places p ON p.id = a.place_id
                JOIN postal_codes c ON c.id = a.postal_code_id
                WHERE c.country_iso2 = ?1 AND c.code_norm = ?2
                ORDER BY a.is_primary DESC, p.level ASC, p.name ASC
                LIMIT ?3
                """,
                bindings: [
                    .text(package.countryCode),
                    .text(normalized),
                    .integer(Int64(QueryLimits.maximumEntities)),
                ],
                map: place
            )
            let primaryIndex = try package.database.first(
                """
                SELECT p.id
                FROM postal_areas a JOIN places p ON p.id = a.place_id
                JOIN postal_codes c ON c.id = a.postal_code_id
                WHERE c.country_iso2 = ?1 AND c.code_norm = ?2 AND a.is_primary = 1
                LIMIT 1
                """,
                bindings: [.text(package.countryCode), .text(normalized)],
                map: { try $0.string(0) }
            )

            return PostalCode(
                code: row.string(0) ?? code,
                countryCode: package.countryCode,
                primaryPlace: areas.first(where: { $0.id == primaryIndex }) ?? areas.first,
                areas: areas,
                coordinates: (latitude != nil && longitude != nil)
                    ? Coordinate(latitude: latitude!, longitude: longitude!)
                    : nil
            )
        }
    }

    static func postalCodes(in package: Package, city: String?, limit: Int) throws -> [PostalCode] {
        let cityFilter: String
        var bindings: [Value] = [.text(package.countryCode), .integer(Int64(min(limit, QueryLimits.maximumEntities)))]

        if let city, !city.isEmpty {
            cityFilter = """
                AND c.id IN (
                    SELECT a.postal_code_id FROM postal_areas a
                    JOIN places p ON p.id = a.place_id
                    WHERE p.name_norm = ?3
                )
                """
            bindings.append(.text(Normalization.normalize(city)))
        } else {
            cityFilter = ""
        }

        let rows = try package.database.query(
            """
            SELECT c.code, c.latitude, c.longitude
            FROM postal_codes c
            WHERE c.country_iso2 = ?1 \(cityFilter)
            ORDER BY c.code
            LIMIT ?2
            """,
            bindings: bindings,
            limit: min(limit, QueryLimits.maximumEntities),
            map: { row in (row.string(0) ?? "", row.double(1), row.double(2)) }
        )

        return try rows.map { code, latitude, longitude in
            let areas = try package.database.query(
                """
                SELECT \(placeColumns), a.is_primary
                FROM postal_areas a JOIN places p ON p.id = a.place_id
                WHERE a.postal_code_id = (SELECT id FROM postal_codes WHERE code_norm = ?1 AND country_iso2 = ?2)
                ORDER BY a.is_primary DESC, p.level ASC, p.name ASC
                LIMIT ?3
                """,
                bindings: [
                    .text(Normalization.postalNormalize(code)),
                    .text(package.countryCode),
                    .integer(Int64(QueryLimits.maximumEntities)),
                ],
                map: place
            )
            return PostalCode(
                code: code,
                countryCode: package.countryCode,
                primaryPlace: areas.first,
                areas: areas,
                coordinates: (latitude != nil && longitude != nil)
                    ? Coordinate(latitude: latitude!, longitude: longitude!)
                    : nil
            )
        }
    }

    // MARK: - Autocomplete

    static func autocomplete(
        _ packages: [Package],
        _ query: String,
        language: Language?,
        limit: Int
    ) throws -> [AddressSuggestion] {
        let capped = min(limit, QueryLimits.autocomplete)
        let results = try search(packages, query, limit: capped)

        return try results.map { result in
            let place = result.place
            // Every field but `text` is optional: spec section 36 requires the model to allow
            // missing levels, and returning a placeholder instead of nil would invent data.
            var parts = [place.name]
            if let parent = place.parent?.name {
                parts.append(parent)
            }

            var localized: String?
            var matchedLanguage: String?
            if let language {
                let names = try Localization.placeNames(place, language: language)
                if let best = names.first(where: { $0.nameClass == .short }) ?? names.first {
                    localized = best.name
                    matchedLanguage = best.language
                }
            }

            return AddressSuggestion(
                text: parts.joined(separator: ", "),
                place: place,
                localizedName: localized,
                language: matchedLanguage,
                postalCode: nil,
                coordinates: place.coordinates
            )
        }
    }

    private static func package(for place: Place) throws -> Package? {
        try Terra.shared().package(for: place.countryCode)
    }

    // MARK: - Nearest

    static func nearest(_ packages: [Package], to coordinate: Coordinate, limit: Int) throws -> [Place] {
        var candidates: [(Place, Double)] = []

        for package in packages {
            // A bounding box around the point, widened as the candidate pool grows. This is a
            // *prefilter*: the R*Tree is a candidate generator, never the answer, because a box
            // prefilter degrades at high latitudes where a degree of longitude is a short
            // distance and the box runs out (ADR-005).
            let box = Self.boundingBox(around: coordinate, kilometres: 50)
            let rows = try package.database.query(
                """
                -- `places_rtree` keys on `places.rowid_int`, not on the textual id: the R*Tree
                -- stores 64-bit floats per row and a text key would be a second, larger payload in
                -- the one index that has to stay small.
                SELECT \(placeColumns)
                FROM places_rtree r JOIN places p ON p.rowid_int = r.id_rowid
                WHERE r.min_lon <= ?1 AND r.max_lon >= ?2
                  AND r.min_lat <= ?3 AND r.max_lat >= ?4
                  AND p.latitude IS NOT NULL AND p.longitude IS NOT NULL
                LIMIT ?5
                """,
                bindings: [
                    .real(box.maxLongitude),
                    .real(box.minLongitude),
                    .real(box.maxLatitude),
                    .real(box.minLatitude),
                    .integer(Int64(QueryLimits.nearestCandidates)),
                ],
                limit: QueryLimits.nearestCandidates,
                map: place
            )
            for place in rows {
                guard let target = place.coordinates else { continue }
                candidates.append((place, coordinate.distance(to: target)))
            }
        }

        // Ordered here, in SDK code, rather than by the database.
        candidates.sort { lhs, rhs in
            if abs(lhs.1 - rhs.1) > 1e-9 { return lhs.1 < rhs.1 }
            return lhs.0.id < rhs.0.id
        }
        return candidates.prefix(max(1, limit)).map(\.0)
    }

    /// A bounding box around a coordinate.
    ///
    /// Longitude degrees shrink with latitude, so the half-width is divided by `cos(latitude)`.
    /// Without that the box is a few times too wide near the poles and too narrow at the equator
    /// for the same distance — which is the concrete way a naive box prefilter fails.
    private static func boundingBox(
        around coordinate: Coordinate,
        kilometres: Double
    ) -> (minLatitude: Double, maxLatitude: Double, minLongitude: Double, maxLongitude: Double) {
        let latitudeDelta = kilometres / 111.32
        let cosLatitude = max(cos(coordinate.latitude * .pi / 180), 0.01)
        let longitudeDelta = kilometres / (111.32 * cosLatitude)
        return (
            max(-90, coordinate.latitude - latitudeDelta),
            min(90, coordinate.latitude + latitudeDelta),
            max(-180, coordinate.longitude - longitudeDelta),
            min(180, coordinate.longitude + longitudeDelta)
        )
    }
}

