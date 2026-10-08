/// Hierarchy traversal, `SDK_SPEC.md` §5.
///
/// **Zero recursive queries.** Every package ships `place_closure` fully materialized, so ancestry
/// is one indexed join ([`ARCHITECTURE.md` §4](../../ARCHITECTURE.md)). A Turkish neighbourhood's
/// `ancestors` returns `[neighborhood, district, province, country]` in four indexed lookups, not
/// a recursive CTE with an unbounded depth guess.
///
/// **Keyset pagination, never `OFFSET`.** `OFFSET 500000` walks half a million rows and throws them
/// away; street lists and large administrative subdivisions hit that immediately.

import Foundation

extension Place {

    /// The package this place came from.
    private var package: Package {
        // Force-unwrapped on purpose: a `Place` only ever comes out of a query against a package,
        // and the resolver caches it. A `Place` whose package has since vanished is a programming
        // error, not a runtime condition to model.
        // swiftlint:disable:next force_unwrapping
        (try! Terra.shared().package(for: countryCode))!
    }

    /// The immediate parent, or `nil` at the top of the hierarchy.
    public var parent: Place? {
        guard let parentID else { return nil }
        return try? Hierarchy.place(id: parentID, in: package)
    }

    /// Every ancestor, root-first, including self.
    ///
    /// Root-first because that is the order a form wants: country, province, district,
    /// neighbourhood.
    public var ancestors: [Place] {
        let rows = try? package.database.query(
            """
            SELECT \(Queries.placeColumns)
            FROM place_closure c JOIN places p ON p.id = c.ancestor_id
            WHERE c.descendant_id = ?1
            ORDER BY c.depth DESC
            """,
            bindings: [.text(id)],
            limit: 8,          // levels are 1...7, so 8 rows including self is the exact bound
            map: Queries.place
        )
        return rows ?? []
    }

    /// Every descendant, ordered by level then name.
    ///
    /// Bounded by `limit`. A country can hold more descendants than any caller wants, and an
    /// unbounded listing is exactly what SDK_SPEC §1.2 forbids.
    public func descendants(limit: Int = QueryLimits.maximumEntities) -> [Place] {
        let rows = try? package.database.query(
            """
            SELECT \(Queries.placeColumns)
            FROM place_closure c JOIN places p ON p.id = c.descendant_id
            WHERE c.ancestor_id = ?1 AND c.depth > 0
            ORDER BY p.level ASC, p.name ASC
            """,
            bindings: [.text(id)],
            limit: min(limit, QueryLimits.maximumEntities),
            map: Queries.place
        )
        return rows ?? []
    }

    /// One page of direct children, keyset-paginated.
    ///
    /// `after` is the last id from the previous page. There is no offset variant, and adding one
    /// would be a regression against SDK_SPEC §5.
    public func children(after cursor: String? = nil, limit: Int = 100) -> Page<Place> {
        let capped = max(1, min(limit, QueryLimits.maximumEntities))
        let sql: String
        var bindings: [Value] = [.text(id)]
        if let cursor {
            // Keyset on (name, id) rather than (id): the ordering is by name, so paging on id
            // would skip or repeat rows whenever two children share a name — which real data does.
            sql = """
                SELECT \(Queries.placeColumns) FROM places p
                WHERE p.parent_id = ?1 AND (p.name, p.id) > (?2, ?2)
                ORDER BY p.name ASC, p.id ASC
                LIMIT ?3
                """
            bindings.append(.text(cursor))
        } else {
            sql = """
                SELECT \(Queries.placeColumns) FROM places p
                WHERE p.parent_id = ?1
                ORDER BY p.name ASC, p.id ASC
                LIMIT ?2
                """
        }
        bindings.append(.integer(Int64(capped + 1)))

        let rows = (try? package.database.query(sql, bindings: bindings, limit: capped + 1, map: Queries.place)) ?? []
        guard rows.count > capped else {
            return Page(elements: rows, nextCursor: nil)
        }
        let page = Array(rows.prefix(capped))
        return Page(elements: page, nextCursor: page.last?.id)
    }

    /// Direct children of type `.district`.
    public var districts: [Place] {
        children(limit: QueryLimits.maximumEntities).elements.filter { $0.type == .district }
    }

    /// The parent's name, or `nil` at the top of the hierarchy.
    ///
    /// Kept separate from `parent` because autocomplete needs the name without materializing the
    /// whole parent row, and the join is already in the index.
    public func parentName() -> String? {
        guard let parentID else { return nil }
        return try? package.database.first(
            "SELECT name FROM places WHERE id = ?1",
            bindings: [.text(parentID)],
            map: { try $0.string(0) }
        ) ?? nil
    }

    /// Every localized name for this place.
    public func names(for language: Language) -> [PlaceName] {
        (try? Localization.placeNames(self, language: language)) ?? []
    }

    /// The best display name in a language, or the place's own name.
    ///
    /// Missing translations are normal, not an error (spec §7), so the place's own name is the
    /// floor rather than an error. Within a language the everyday form wins over the formal one:
    /// `names(for:)` is already ordered by `PlaceNameClass.displayRank`, so the first entry is the
    /// answer and there is no second rule here to get out of step with it.
    public func localizedName(for language: Language) -> String {
        names(for: language).first?.name ?? name
    }
}

extension Country {
    /// The country's place row, which carries the hierarchy anchor at level 1.
    public var place: Place? {
        try? Hierarchy.countryPlace(self)
    }

    /// Every localized name for this country.
    public func names(for language: Language) -> [PlaceName] {
        guard let place else { return [] }
        return (try? Localization.placeNames(place, language: language)) ?? []
    }

    /// The best display name in a language.
    ///
    /// `country("TR").name` is `Türkiye` rather than GeoNames' English `Turkey`, because the
    /// pipeline promotes a display name into the country row. `localizedName(for:)` returns the name
    /// in the requested language where the package holds one, and the promoted endonym otherwise —
    /// the fallback chain's floor (`SDK_SPEC.md` §7).
    public func localizedName(for language: Language) -> String {
        names(for: language).first?.name ?? name
    }
}

/// Queries shared by the hierarchy and localization extensions.
enum Hierarchy {

    static func place(id: String, in package: Package) throws -> Place? {
        try package.database.first(
            "SELECT \(Queries.placeColumns) FROM places p WHERE p.id = ?1",
            bindings: [.text(id)],
            map: Queries.place
        )
    }

    static func countryPlace(_ country: Country) throws -> Place? {
        guard let package = try Terra.shared().package(for: country.iso2) else { return nil }
        return try package.database.first(
            "SELECT \(Queries.placeColumns) FROM places p WHERE p.country_iso2 = ?1 AND p.level = 1",
            bindings: [.text(country.iso2)],
            map: Queries.place
        )
    }
}

/// Localized names, `SDK_SPEC.md` §7.
///
/// Two sources, and the distinction matters:
///
/// * `place_names` — localized, with the preferred/short flags GeoNames provides
/// * `place_alternate_names` — exonyms, historic names and transliterations
///
/// `place_alternate_names` is why `localizedName(for: .japanese)` on İstanbul returns
/// `イスタンブール`, and why searching `Constantinople` finds Istanbul.
enum Localization {

    /// Names for a place in the requested language, ordered for display.
    ///
    /// **The chain stops at the first rung that answers.** `SDK_SPEC.md` §7 defines the fallback as
    /// `requested language -> locale fallback -> "en" -> local name`, and a chain that kept walking
    /// past the first answer is not a fallback chain — it is a union. Merging the rungs and sorting
    /// by name class made the English `short` name outrank the German `preferred` one, so
    /// `localizedName(for: .german)` on Türkiye returned `Türkiye` while the package held `Türkei`
    /// one rung earlier.
    static func placeNames(_ place: Place, language: Language) throws -> [PlaceName] {
        guard let package = try Terra.shared().package(for: place.countryCode) else { return [] }

        for tag in language.fallbackChain {
            // Localized names, with the preferred/short flags GeoNames provides.
            var names = try package.database.query(
                """
                SELECT lang, name, script, is_preferred, is_short
                FROM place_names
                WHERE place_id = ?1 AND lang = ?2
                """,
                bindings: [.text(place.id), .text(tag)],
                limit: 32,
                map: { row in
                    let isShort = (row.int(4) ?? 0) == 1
                    let isPreferred = (row.int(3) ?? 0) == 1
                    return PlaceName(
                        name: row.string(1) ?? "",
                        script: row.string(2),
                        // Read `lang` back rather than assuming the tag: a package is free to
                        // record a more specific tag than the one that was queried, and reporting
                        // the assumed value would mislabel it.
                        language: row.string(0) ?? tag,
                        nameClass: isShort ? .short : (isPreferred ? .preferred : .short)
                    )
                }
            )

            // Alternates in the same language: exonyms, historic names and transliterations.
            // Included because a caller asking for a language wants what that language calls the
            // place. `lang` is nullable here, and a name with no recorded language is not a
            // localized name — it is reachable through search, not through this.
            names += try package.database.query(
                """
                SELECT lang, name, class FROM place_alternate_names
                WHERE place_id = ?1 AND lang = ?2
                """,
                bindings: [.text(place.id), .text(tag)],
                limit: 32,
                map: { row in
                    PlaceName(
                        name: row.string(1) ?? "",
                        script: nil,
                        language: row.string(0) ?? tag,
                        nameClass: PlaceNameClass(packageClass: row.string(2))
                    )
                }
            )

            guard !names.isEmpty else { continue }

            // Deduplicated by text, then ordered for display. The class decides, and the text
            // breaks ties so the order is total and reproducible rather than dependent on which
            // table the row came from.
            var seen = Set<String>()
            return names
                .filter { !$0.name.isEmpty && seen.insert($0.name).inserted }
                .sorted { lhs, rhs in
                    if lhs.nameClass != rhs.nameClass {
                        return lhs.nameClass.displayRank < rhs.nameClass.displayRank
                    }
                    return lhs.name < rhs.name
                }
        }

        // Every rung of the chain was empty. Missing translations are normal, so this is an empty
        // result, not an error, and the caller's own name is the floor.
        return []
    }
}
