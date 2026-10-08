import XCTest

@testable import TerraKit

/// Cross-implementation conformance, `SDK_SPEC.md` §14.
///
/// Runs the shared battery from `tests/integration/conformance/battery.json` against the same
/// package bytes the CLI ran, and compares **result ID sequences**, not counts. A count can match
/// while the order does not, and the user experiences that as "why is the right answer fourth".
///
/// The expectations come from the Rust CLI, which produced the packages and is therefore the
/// reference. Where the CLI has no equivalent — `nearest`, `autocomplete`, `ancestors`, `children`,
/// `localizedName` — the expectation is recorded by this SDK into `expected/swift.json` and the
/// Kotlin SDK is then held to it. That asymmetry is stated rather than hidden: the CLI simply does
/// not expose those reads.
///
/// Requires the packages. Skipped with a printed reason rather than failing when they are absent, so
/// `swift test` works on a checkout with no data — but the skip is never silent.
///
/// ```bash
/// TERRAKIT_CONFORMANCE_PACKAGES=<dir> swift test --filter Conformance
/// ```
final class ConformanceTests: XCTestCase {

    // MARK: - Fixtures

    private struct Battery: Decodable {
        struct Query: Decodable {
            let id: String
            let `op`: String
            let query: String?
            let code: String?
            let country: String?
            let city: String?
            let language: String?
            let latitude: Double?
            let longitude: Double?
            let place: String?
            let limit: Int?
            let why: String?
        }
        let version: Int
        let queries: [Query]
    }

    private struct Expectations: Decodable {
        let results: [String: Result]

        struct Result: Decodable {
            let ids: [String]?
            let names: [String]?
            let unavailable: String?
        }
    }

    private static var repositoryRoot: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // TerraKitTests
            .deletingLastPathComponent()   // Tests
            .deletingLastPathComponent()   // swift
            .deletingLastPathComponent()   // sdk
            .deletingLastPathComponent()   // repository root
    }

    private static var conformanceDirectory: URL {
        repositoryRoot.appendingPathComponent("tests/integration/conformance")
    }

    /// Where the CLI recorded its expectations.
    private static var cliExpectationsURL: URL {
        conformanceDirectory.appendingPathComponent("expected/cli.json")
    }

    /// Where this SDK records the queries the CLI cannot run.
    private static var sdkExpectationsURL: URL {
        conformanceDirectory.appendingPathComponent("expected/swift.json")
    }

    private static var packagesDirectory: URL? {
        guard let raw = ProcessInfo.processInfo.environment["TERRAKIT_CONFORMANCE_PACKAGES"] else {
            return nil
        }
        let url = URL(fileURLWithPath: raw)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    // MARK: - Setup

    override func setUpWithError() throws {
        try super.setUpWithError()
        guard let packages = Self.packagesDirectory else {
            throw XCTSkip("""
                TERRAKIT_CONFORMANCE_PACKAGES is not set, so there are no packages to be \
                conformant about. Build them with the pipeline, then re-run:
                  TERRAKIT_CONFORMANCE_PACKAGES=<dir> swift test --filter Conformance
                """)
        }
        try Terra.configure(packageDirectory: packages)
    }

    // MARK: - Helpers

    private func battery() throws -> Battery {
        let data = try Data(contentsOf: Self.conformanceDirectory.appendingPathComponent("battery.json"))
        return try JSONDecoder().decode(Battery.self, from: data)
    }

    private func cliExpectations() throws -> Expectations {
        let url = Self.cliExpectationsURL
        guard FileManager.default.fileExists(atPath: url.path) else {
            throw XCTSkip("""
                \(url.lastPathComponent) is missing. Produce it with:
                  python3 tools/conformance.py generate --packages <dir>
                """)
        }
        return try JSONDecoder().decode(Expectations.self, from: Data(contentsOf: url))
    }

    /// Resolve the battery's symbolic placeholders.
    ///
    /// `"firstCity"` and `"country"` rather than a hard-coded geonamesid, because a fixture that
    /// pins an id breaks the moment the pipeline rebuilds a country with different upstream data —
    /// and then the suite fails for a reason that has nothing to do with conformance.
    private func resolvePlace(_ token: String, country: String) throws -> Place {
        let package = try Terra.shared().requirePackage(country)
        switch token {
        case "country":
            guard let place = try package.database.first(
                "SELECT \(Queries.placeColumns) FROM places p WHERE p.country_iso2 = ?1 AND p.level = 1",
                bindings: [.text(country)],
                map: Queries.place
            ) else {
                throw TerraError.packageNotInstalled(country: country)
            }
            return place
        case "firstCity":
            guard let place = try package.database.first(
                """
                SELECT \(Queries.placeColumns) FROM places p
                WHERE p.country_iso2 = ?1 AND p.type IN ('city','town')
                ORDER BY p.level ASC, p.name ASC
                LIMIT 1
                """,
                bindings: [.text(country)],
                map: Queries.place
            ) else {
                throw TerraError.packageNotInstalled(country: country)
            }
            return place
        default:
            guard let place = try Hierarchy.place(id: token, in: package) else {
                throw TerraError.databaseUnreadable(underlying: "no place with id \(token)")
            }
            return place
        }
    }

    /// Run one battery query and return its ID sequence.
    private func run(_ query: Battery.Query) throws -> [String]? {
        let limit = query.limit ?? 20
        switch query.op {
        case "search":
            let results = try Terra.search(
                query.query ?? "",
                country: query.country,
                limit: limit
            )
            return results.map(\.place.id)

        case "searchCity":
            return try Terra.searchCity(
                query.query ?? "",
                country: query.country,
                limit: limit
            ).map(\.id)

        case "country":
            guard let country = try Terra.country(query.country ?? "") else { return [] }
            // A field-by-field sequence, so a divergence names the field rather than dumping two
            // structs a reader has to diff by eye.
            return [country.iso2, country.iso3, country.name, country.capitalPlaceID ?? "-"]

        case "postalCode":
            guard let code = try Terra.postalCode(query.code ?? "", country: query.country) else {
                return []
            }
            return code.areas.map(\.id)

        case "nearest":
            guard let latitude = query.latitude, let longitude = query.longitude else { return [] }
            let found = try Terra.nearest(
                to: Coordinate(latitude: latitude, longitude: longitude),
                country: query.country,
                limit: limit
            )
            return found.map(\.id)

        case "autocomplete":
            return try Terra.autocomplete(
                query.query ?? "",
                country: query.country,
                limit: limit
            ).map { $0.place?.id ?? "-" }

        case "ancestors":
            let place = try resolvePlace(query.place ?? "", country: query.country ?? "TR")
            return place.ancestors.map(\.id)

        case "children":
            let place = try resolvePlace(query.place ?? "", country: query.country ?? "TR")
            return place.children(limit: limit).elements.map(\.id)

        case "localizedName":
            let language = Language(bcp47: query.language ?? "en") ?? .english
            if query.place == "country" {
                guard let country = try Terra.country(query.country ?? "") else { return [] }
                return [country.localizedName(for: language)]
            }
            let place = try resolvePlace(query.place ?? "", country: query.country ?? "TR")
            return [place.localizedName(for: language)]

        default:
            return nil
        }
    }

    // MARK: - Tests

    /// The CLI's recorded results, reproduced exactly.
    ///
    /// The core assertion. Every query the CLI can run must return the same IDs in the same order.
    func testMatchesTheCLIRecordedExpectations() throws {
        let battery = try battery()
        let expectations = try cliExpectations()

        var failures: [String] = []
        var compared = 0

        for query in battery.queries {
            guard let expected = expectations.results[query.id] else {
                failures.append("\(query.id): the CLI recorded no expectation for it")
                continue
            }
            guard expected.unavailable == nil, let want = expected.ids else {
                continue   // no CLI equivalent; covered by testSDKOnlyQueriesAreStable
            }

            let got = try run(query)
            compared += 1
            if got != want {
                failures.append(
                    """
                    \(query.id) — \(query.why ?? "")
                        CLI:     \(want)
                        Swift:   \(got ?? [])
                    """
                )
            }
        }

        XCTAssertGreaterThan(compared, 0, "the battery matched no queries at all")
        XCTAssertTrue(
            failures.isEmpty,
            "\(failures.count)/\(compared) divergence(s):\n\n" + failures.joined(separator: "\n")
        )
    }

    /// The queries the CLI cannot run are stable, and are recorded for Kotlin to be held to.
    func testSDKOnlyQueriesAreStable() throws {
        let battery = try battery()
        let cli = try cliExpectations()

        var recorded: [String: [String]] = [:]
        var failures: [String] = []

        for query in battery.queries {
            guard let expected = cli.results[query.id], expected.unavailable != nil else { continue }
            guard let got = try run(query) else { continue }

            recorded[query.id] = got
            if let previous = try? recordedExpectations()[query.id] {
                if previous != got {
                    failures.append(
                        """
                        \(query.id) — \(query.why ?? "")
                            recorded: \(previous)
                            now:      \(got)
                        """
                    )
                }
            }
        }

        if !failures.isEmpty {
            XCTFail("\(failures.count) unstable SDK-only result(s):\n\n" + failures.joined(separator: "\n"))
        }

        // Written so the Kotlin SDK has something to be held to. Not written when a recorded file
        // already exists, so an ordinary test run cannot silently bless a changed result.
        let url = Self.sdkExpectationsURL
        if !FileManager.default.fileExists(atPath: url.path), !recorded.isEmpty {
            let document: [String: Any] = [
                "$comment": """
                    Produced by the Swift SDK for the queries the CLI does not expose \
                    (SDK_SPEC.md section 14). Kotlin is held to these. If a change here is \
                    intended, delete the file deliberately -- do not let a test run rewrite it.
                    """,
                "results": recorded,
            ]
            let data = try JSONSerialization.data(
                withJSONObject: document,
                options: [.prettyPrinted, .sortedKeys]
            )
            try data.write(to: url)
            print("recorded \(recorded.count) SDK-only expectations to \(url.lastPathComponent)")
        }
    }

    private func recordedExpectations() throws -> [String: [String]] {
        let url = Self.sdkExpectationsURL
        guard FileManager.default.fileExists(atPath: url.path) else { return [:] }
        guard let object = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any],
              let results = object["results"] as? [String: [String]]
        else {
            return [:]
        }
        return results
    }

    /// The documented CJK limitation is asserted, so it cannot be "fixed" into a scan.
    func testTheTrigramFloorIsDocumentedBehaviour() throws {
        // `東京都` is three characters and the trigram index serves it. `東京` is two and cannot be
        // indexed; asking anyway would be a full scan, which spec section 41 forbids. The
        // limitation is recorded rather than papered over (DATA_MODEL.md section 9).
        let threeCharacter = try Terra.search("東京都", country: "JP", limit: 10)
        XCTAssertFalse(threeCharacter.isEmpty, "three characters must reach the trigram index")

        let twoCharacter = try Terra.search("東京", country: "JP", limit: 10)
        XCTAssertTrue(
            twoCharacter.isEmpty,
            "two characters must return nothing rather than scan; got \(twoCharacter.map(\.place.id))"
        )
    }

    /// A missing package is a typed error, not an empty result.
    ///
    /// Silently returning nothing for an uninstalled country is the failure that ships a bug nobody
    /// notices until a customer reports an empty dropdown.
    func testAMissingPackageThrowsRatherThanReturningEmpty() throws {
        XCTAssertThrowsError(try Terra.country("ZZ")) { error in
            guard case TerraError.packageNotInstalled(let country) = error else {
                return XCTFail("expected .packageNotInstalled, got \(error)")
            }
            XCTAssertEqual(country, "ZZ")
        }
    }

    /// A feature the package does not provide is its own error.
    func testAnUnavailableFeatureThrowsRatherThanReturningEmpty() throws {
        // Streets need OSM, whose ODbL share-alike would constrain every downstream user, so they
        // are outside the MVP (ADR-004). Returning an empty array would look like "no streets
        // match" — a silent, plausible, wrong answer.
        XCTAssertThrowsError(try Terra.searchStreet("Atatürk", country: "TR")) { error in
            guard case TerraError.featureUnavailable(let feature, _) = error else {
                return XCTFail("expected .featureUnavailable, got \(error)")
            }
            XCTAssertEqual(feature, "streets")
        }
    }

    /// Ordering is total: the same query twice returns the same sequence.
    func testSearchOrderingIsStable() throws {
        let first = try Terra.search("Antalya", country: "TR", limit: 20).map(\.place.id)
        let second = try Terra.search("Antalya", country: "TR", limit: 20).map(\.place.id)
        XCTAssertEqual(first, second)
        XCTAssertFalse(first.isEmpty)
    }

    /// One place, one result — even when it matches through several indexed names.
    func testAPlaceMatchingSeveralNamesYieldsOneResult() throws {
        // The Turkish province matches "Antalya" through its own name, "Ili", the Basque
        // "probintzia" and Georgian transliterations. The CLI learned this the hard way; the SDK
        // must not repeat it.
        let results = try Terra.search("Antalya", country: "TR", limit: 50)
        let ids = results.map(\.place.id)
        XCTAssertEqual(Set(ids).count, ids.count, "a place appeared more than once: \(ids)")
    }

    /// Every public entry point is covered.
    ///
    /// `Terra.city(_:country:)` shipped with an unquoted `IN (city,town)`, which SQLite reads as two
    /// column references, so every call failed with "no such column: city". The battery did not
    /// catch it because `searchCity` is a *separate* query that spells its own literals correctly —
    /// two entry points, one covered. This walks the surface so a new entry point cannot arrive
    /// untested the same way.
    func testEveryEntryPointIsReachable() throws {
        // Each of these must return without throwing for a country that is installed.
        XCTAssertFalse(try Terra.country("TR") == nil)
        XCTAssertFalse(try Terra.countries().isEmpty)
        XCTAssertFalse(try Terra.city("Antalya", country: "TR").isEmpty)
        XCTAssertFalse(try Terra.search("Antalya", country: "TR").isEmpty)
        XCTAssertFalse(try Terra.searchCity("Antalya", country: "TR").isEmpty)
        XCTAssertFalse(try Terra.postalCode("07160", country: "TR") == nil)
        XCTAssertFalse(try Terra.postalCodes(country: "TR").isEmpty)
        XCTAssertFalse(try Terra.autocomplete("Ant", country: "TR").isEmpty)
        XCTAssertFalse(
            try Terra.nearest(
                to: Coordinate(latitude: 36.8969, longitude: 30.7133),
                country: "TR"
            ).isEmpty
        )
        XCTAssertFalse(try Terra.attribution().isEmpty)
        XCTAssertFalse(try Terra.attributionString().isEmpty)
        XCTAssertFalse(try Terra.license(for: "TR") == nil)
        XCTAssertFalse(try Terra.capabilities().countries.isEmpty)

        let city = try XCTUnwrap(try Terra.city("Antalya", country: "TR").first)
        XCTAssertNotNil(city.parent)
        XCTAssertFalse(city.ancestors.isEmpty)
        _ = city.children(limit: 5)
        _ = city.descendants(limit: 5)
        _ = city.districts
        _ = city.names(for: .english)
    }

    /// `city(_:country:)` is exact and narrow; `searchCity` is ranked.
    ///
    /// The two differ on purpose and the difference is asserted, because having one call do
    /// "cities named X" and another do "cities that look like X" is how two consumers end up with
    /// different answers for the same question.
    func testCityIsExactWhereSearchCityIsRanked() throws {
        let exact = try Terra.city("Antalya", country: "TR")
        XCTAssertTrue(exact.allSatisfy { $0.type.isSettlement })
        XCTAssertTrue(exact.allSatisfy { $0.nameNormalized == "antalya" })

        // A prefix of the name is not an exact match, so `city` must not return it.
        XCTAssertTrue(try Terra.city("Antal", country: "TR").isEmpty)
    }

    /// Localized names resolve in every language, `SDK_SPEC.md` §7.
    ///
    /// The fallback chain ends in the local name, so a *missing* translation is indistinguishable
    /// from a *broken* lookup if only the fallback result is ever checked. These languages are all
    /// present in the Turkish package, so returning the local name would be a defect rather than
    /// the documented fallback.
    func testLocalizedNamesResolveInEveryLanguage() throws {
        guard let country = try Terra.country("TR") else {
            return XCTFail("the TR package should yield a country")
        }
        XCTAssertEqual(country.localizedName(for: .german), "Türkei")
        XCTAssertEqual(country.localizedName(for: .japanese), "トルコ")
        XCTAssertEqual(country.localizedName(for: .turkish), "Türkiye")
    }

    /// The everyday form outranks the formal one, wherever it is stored.
    ///
    /// GeoNames flags both and they disagree: `tr:Türkiye` carries `is_short` while
    /// `en:Republic of Türkiye` is only `is_preferred`. Showing the formal one has shipped
    /// `Bundesrepublik Deutschland` into a dropdown, so the everyday form must win.
    ///
    /// The everyday form is not always in `place_names`. Türkiye's English everyday name arrives as
    /// an `en/short` row in `place_alternate_names`, so an implementation that reads only
    /// `place_names` returns `Republic of Türkiye` for an English-speaking user — correct for a
    /// query, wrong for a dropdown. Both tables are therefore searched here.
    ///
    /// Asserted as a property over every language the package carries rather than as three
    /// hard-coded strings, so it holds for all 245 and not just the ones the battery names.
    func testTheEverydayFormOutranksTheFormalOne() throws {
        guard let country = try Terra.country("TR"),
              let place = try Hierarchy.countryPlace(country)
        else {
            return XCTFail("the TR country row should resolve")
        }
        let package = try Terra.shared().requirePackage("TR")

        var checked = 0
        for language in Language.allCases {
            // Every everyday name this place has in this language, from either table.
            var everyday: [String] = try package.database.query(
                "SELECT name FROM place_names WHERE place_id = ?1 AND lang = ?2 AND is_short = 1",
                bindings: [.text(place.id), .text(language.rawValue)],
                limit: 8,
                map: { $0.string(0) ?? "" }
            )
            everyday += try package.database.query(
                """
                SELECT name FROM place_alternate_names
                WHERE place_id = ?1 AND lang = ?2 AND class = 'short'
                """,
                bindings: [.text(place.id), .text(language.rawValue)],
                limit: 8,
                map: { $0.string(0) ?? "" }
            )
            guard !everyday.isEmpty else { continue }
            checked += 1

            XCTAssertTrue(
                everyday.contains(country.localizedName(for: language)),
                """
                \(language.rawValue): the package holds the everyday form(s) \(everyday) \
                but localizedName returned \(country.localizedName(for: language))
                """
            )
        }
        // Only two of `Language.allCases` have an everyday form for Türkiye — `tr` (the endonym) and
        // `en` (an alternate the Turkish package carries) — because GeoNames flags `is_short` for
        // almost nothing. The floor is therefore 2, not the 17 languages the enum offers: a
        // threshold above the real count would fail the moment upstream data improves, which is the
        // wrong way for a test to break.
        XCTAssertGreaterThanOrEqual(checked, 2, "the property was barely exercised")
    }

    /// Türkiye's English everyday name is an alternate, and it beats the formal name.
    ///
    /// The concrete case behind the test above, kept separately because it is the one that breaks:
    /// `place_names` holds no English everyday name, so only an implementation that also reads
    /// `place_alternate_names` produces `Türkiye` rather than `Republic of Türkiye`.
    func testTheEnglishEverydayNameComesFromTheAlternateTable() throws {
        guard let country = try Terra.country("TR") else {
            return XCTFail("the TR package should yield a country")
        }
        XCTAssertEqual(country.localizedName(for: .english), "Türkiye")
        XCTAssertEqual(country.localizedName(for: .german), "Türkei")
        XCTAssertEqual(country.localizedName(for: .japanese), "トルコ")
        XCTAssertEqual(country.localizedName(for: .turkish), "Türkiye")

        // The English everyday name is not in place_names at all, which is what makes this a
        // real check on the alternate-table path rather than a restatement of the first assertion.
        let package = try Terra.shared().requirePackage("TR")
        guard let place = try Hierarchy.countryPlace(country) else {
            return XCTFail("the TR country row should resolve")
        }
        let fromLocalized = try package.database.first(
            "SELECT name FROM place_names WHERE place_id = ?1 AND lang = 'en' AND is_short = 1",
            bindings: [.text(place.id)],
            map: { $0.string(0) ?? "" }
        )
        XCTAssertNil(fromLocalized, "place_names unexpectedly gained an English everyday name")
    }

    /// Every localized name carries the language it is actually in.
    func testLocalizedNamesReportTheirOwnLanguage() throws {
        guard let country = try Terra.country("TR") else {
            return XCTFail("the TR package should yield a country")
        }
        for name in country.names(for: .german) {
            XCTAssertTrue(
                name.language == "de" || name.language == "de-001",
                "a name requested in German reports \(name.language)"
            )
        }
    }

    /// A query under the trigram floor still finds a Latin-script prefix match.
    ///
    /// `An` is two characters, so the trigram mechanism cannot run and the short-query path is
    /// exact-or-prefix. Every result must therefore be a prefix match, and none may be fuzzy — a
    /// fuzzy result here would mean the mechanism ran despite being unable to index the query.
    ///
    /// `Ant` would be the wrong query to use: three characters clears the floor, so fuzzy runs and
    /// legitimately returns Turkey itself via the transliteration "an tuirc". That is correct
    /// behaviour, and asserting "everything is a prefix" for it fails for the right reason.
    func testShortQueriesUseThePrefixIndex() throws {
        let results = try Terra.search("An", country: "TR", limit: 20)
        XCTAssertFalse(results.isEmpty, "a two-character prefix must still match")

        let names = results.map(\.place.nameNormalized)
        XCTAssertTrue(
            names.allSatisfy { $0.hasPrefix("an") },
            "a short query must only produce prefix matches, got \(names)"
        )
        XCTAssertFalse(
            results.contains { $0.matchKind == .fuzzy },
            "the trigram mechanism must not run below the floor"
        )
        XCTAssertTrue(
            names.contains { $0 == "ankara" } && names.contains { $0 == "antalya" },
            "expected both Turkish cities reached by prefix, got \(names)"
        )
    }

    /// A query at the trigram floor does reach the fuzzy mechanism.
    ///
    /// The counterpart to the test above, and the reason the floor is three rather than four: at
    /// exactly three characters a trigram exists and the index is usable.
    func testThreeCharactersClearTheTrigramFloor() throws {
        let results = try Terra.search("Ant", country: "TR", limit: 20)
        XCTAssertTrue(
            results.contains { $0.matchKind == .fuzzy },
            "three characters is exactly the floor and must reach the fuzzy mechanism"
        )
    }

    /// Dots and wildcards in a query are literal, not syntax.
    func testQueryWildcardsAreEscaped() throws {
        // Without escaping, `Ant_ya` returns every six-letter place starting with "Ant" and the
        // user has no way to know why.
        let results = try Terra.search("Ant_ya", country: "TR", limit: 20)
        XCTAssertTrue(
            results.allSatisfy { !$0.place.nameNormalized.contains("_") },
            "an underscore was treated as a single-character wildcard"
        )
    }
}