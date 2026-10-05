# TerraKit SDK Specification

Status: **Draft — surface defined, not implemented**
Last updated: 2026-10-05
Targets: Swift (SPM) and Kotlin (Gradle, JVM + Android)

Both SDKs expose the **same API in the same shape**. A feature added to one and
not the other is a defect, not a platform limitation. The only permitted
divergence is naming convention.

---

## 1. Principles

These are not preferences. Each corresponds to a rule in
[`ARCHITECTURE.md`](ARCHITECTURE.md) that a different design would violate.

1. **Offline by construction.** The SDK opens a read-only SQLite file. It is the only data
   source it has. No query path contains a network branch.
2. **Nothing unbounded is public.** Every list-returning method takes a limit. There is no
   `allStreets`.
3. **Results are ordered and that order is stable.** Same package bytes, same result order,
   in the CLI and both SDKs.
4. **Errors are typed, not optional.** A missing package and an empty result are different
   outcomes and must not look alike.
5. **IDs are opaque.** `String`, never parsed.
6. **Zero third-party dependencies** beyond `libsqlite3` and the standard library
   ([`ADR-003`](docs/adr/ADR-003-search-mechanisms.md), [`ADR-007`](docs/adr/ADR-007-language-stack.md)).

---

## 2. Package resolution

Where packages are looked for, in order. First match wins; resolved once and cached.

```text
1. explicit configuration        Terra.configure(packages: [...])
2. the application bundle        iOS/macOS resource, Android assets
3. .terrakit/packages/…          local development
```

Step 2 is what makes `import TerraKit` work in a shipped app, since an app reads
only from its bundle. `terrakit bundle --platform ios`
([`ARCHITECTURE.md` §6](ARCHITECTURE.md#6-mobile-packaging)) generates the resource
manifest for it.

```swift
// Explicit, for a test fixture or an unusual layout
try Terra.configure(packageDirectory: url)

// Discovery: bundle first, then .terrakit/
try Terra.open()
```

```kotlin
Terra.configure(packageDirectory = directory)   // explicit
Terra.open()                                  // discovery
```

A package that is absent produces `.packageNotInstalled`, not an empty result.
Silently returning nothing for an uninstalled country is the failure mode that
ships a bug nobody notices until a customer reports an empty dropdown.

---

## 3. The `Terra` namespace

The entry points SPEC §50 and §51 name. Each is a thin wrapper over one indexed
query, so the query plans below are the contract.

```swift
let countries = Terra.countries                      // all installed countries
let turkey    = Terra.country("TR")                 // by ISO 3166-1 alpha-2
let antalya   = Terra.city("Antalya")               // by name, anywhere
let antalyaTR = Turkey.city("Antalya")              // scoped to one country
```

```kotlin
val countries = Terra.countries
val turkey    = Terra.country("TR")
val antalya   = Terra.city("Antalya")
val antalyaTR = turkey.city("Antalya")
```

| Call | Query |
|---|---|
| `countries` | `SELECT … FROM countries` |
| `country(_:)` | `… WHERE countries.iso2 = ?` |
| `city(_:)` | `… WHERE places.name_norm = ? AND type IN ('city','town')` |
| `search(_:)` | exact → prefix → trigram, ordered by rank |
| `searchCity(_:)` | as `city`, `ORDER BY search_rank DESC` |
| `searchStreet(_:)` | `search_fts MATCH ?` — unavailable without a `streets` package |
| `searchPostalCode(_:)` | `… WHERE postal_codes.code_norm = ?` |
| `autocomplete(_:)` | `search_fts MATCH ?` + rank, capped at 50 |
| `nearest(latitude:longitude:)` | `places_rtree` candidates, haversine in SDK code |
| `postalCode(_:)` | `postal_codes` → `postal_areas` → places |
| `postalCodes(country:city:)` | places → `postal_areas` → `postal_codes` |

`name_norm` in a query is the **normalized form of the user's input**, computed by
the SDK's own copy of [`core/spec/normalization.md`](core/spec/normalization.md).
That one function is the only normalization an SDK performs; phonetic codes and
transliterations are read from the package
([`ADR-003`](docs/adr/ADR-003-search-mechanisms.md)).

---

## 4. Types

### 4.1 Core entities

```swift
public struct Country: Identifiable, Hashable, Sendable {
    public let id: String                 // 'country:TR' — opaque
    public let iso2: String               // 'TR'
    public let iso3: String               // 'TUR'
    public let numericCode: String        // '792'
    public let name: String               // English, from the package
    public let localizedName: String      // resolved via `localizedName(for:)`
    public let continentCode: String      // 'AS'
    public let capitalPlaceID: String?
    public let currencyCode: String       // 'TRY'  (ISO 4217)
    public let phoneCode: String?         // '+90'
    public let internetTLD: String?       // '.tr'
    public let drivingSide: DrivingSide? // .left / .right
    public let timezones: [String]        // IANA ids: ['Europe/Istanbul']
    public let languageCodes: [String]
    public let coordinates: Coordinate?
}
```

```kotlin
public data class Country(
    val id: String,
    val iso2: String,
    val iso3: String,
    val numericCode: String,
    val name: String,
    val continentCode: String,
    val capitalPlaceID: String?,
    val currencyCode: String,
    val phoneCode: String?,
    val internetTLD: String?,
    val drivingSide: DrivingSide?,
    val timezones: List<String>,
    val languageCodes: List<String>,
    val coordinates: Coordinate?,
)
```

### 4.2 `Place` — one type for the whole hierarchy

```swift
public struct Place: Identifiable, Hashable, Sendable {
    public let id: String
    public let type: PlaceType            // semantic: .city, .district, .province
    public let level: Int                 // structural: 1…7
    public let countryCode: String
    public let parentID: String?
    public let name: String
    public let nameNormalized: String
    public let administrativeCode: String?  // 'TR-07' where ISO 3166-2 applies
    public let coordinates: Coordinate?
    public let population: Int?
    public let timezoneID: String?          // 'Europe/Istanbul', IANA
    public let currencyCode: String?
}
```

`type` and `level` are separate and both public. Conflating them is how address
libraries break: Türkiye's district is at level 3, the UK's at level 4, and Japan
has no county anywhere
([`DATA_MODEL.md` §1](DATA_MODEL.md#1-core-principle-type-and-level-are-different-things)).

```swift
public enum PlaceType: String, Sendable {
    case country, region, state, province, prefecture, canton, department
    case county, district, municipality, borough, ward, suburb
    case city, town, village, neighborhood
}
```

### 4.3 Coordinate

```swift
public struct Coordinate: Hashable, Sendable {
    public let latitude: Double
    public let longitude: Double

    /// Haversine, in kilometres. The SDK never returns an offset or a zone offset
    /// as a Date: timezones are IANA ids, resolved by the platform
    /// (DATA_MODEL.md section 10).
    public func distance(to other: Coordinate) -> Double
}
```

---

## 5. Hierarchy traversal

Zero recursive queries. Every package ships `place_closure` fully materialized,
so ancestry is one indexed join
([`ARCHITECTURE.md` §4](ARCHITECTURE.md#4-runtime-query-strategy)).

```swift
place.ancestors            // [Place] root-first, including self
place.descendants          // [Place] everything below, ordered by level then name
place.parent               // Place?
place.children(limit: 100) // [Place]
place.districts            // [Place] children of type .district
```

```kotlin
place.ancestors
place.descendants
place.parent
place.children(limit = 100)
place.districts
```

`ancestors` on a Turkish neighbourhood returns
`[neighborhood, district, province, country]` — four indexed lookups, not a
recursive CTE with an unbounded depth guess.

Pagination is keyset, never `OFFSET`. `OFFSET 500000` walks half a million rows
and throws them away; street lists and large administrative subdivisions hit that
immediately.

```swift
func children(after cursor: String?, limit: Int = 100) -> Page<Place>
```

---

## 6. Search

```swift
public struct SearchResult: Sendable {
    public let place: Place
    public let matchKind: MatchKind      // .exact, .prefix, .fuzzy
    public let score: Double             // 1.0 exact … 0.0 fuzzy floor
}

public enum MatchKind: Int, Comparable, Sendable { case exact, prefix, fuzzy }
```

Ordering is total and deterministic, so a conformance test can compare result
**sets** rather than counts:

1. `MatchKind` (exact before prefix before fuzzy)
2. fuzzy: Double Metaphone distance, ascending
3. `search_rank` descending — precomputed by the pipeline
4. `id` ascending — the final tiebreak, which is what makes the order total

```swift
Terra.search("Antalya", country: "TR", limit: 20)              // [SearchResult]
Terra.searchCity("Antalya")                                    // [Place]
Terra.search("Antaliya", fuzzy: true)                         // typo tolerance
Terra.search("Istanbul", language: .english)                  // localized + exonyms
```

**Query routing is documented, not incidental.** Queries of 3+ characters go to
the FTS5 trigram index; shorter ones go to the B-tree prefix index, because a
trigram tokenizer cannot index a 2-character token
([`DATA_MODEL.md` §9](DATA_MODEL.md#9-search-schema)). Same answer either way,
different plan.

### 6.1 Autocomplete

```swift
public struct AddressSuggestion: Sendable {
    public let text: String                 // "Atatürk Caddesi, Muratpaşa, Antalya"
    public let street: Place?
    public let district: Place?
    public let city: Place?
    public let province: Place?
    public let country: Country?
    public let postalCode: String?
    public let coordinates: Coordinate?
}
```

Every field except `text` is optional, because SPEC §36 requires the model to
allow missing levels — not every country provides every one. Returning a
placeholder instead of `nil` would invent data.

Default cap: 50 results. The SDK never returns an unbounded list, because
autocomplete runs on every keystroke.

### 6.2 Nearest

```swift
Terra.nearest(latitude: 36.8969, longitude: 30.7133, limit: 10)
Terra.nearest(to: coordinate, within: .kilometers(5), limit: 10)
```

Candidates come from `places_rtree`; exact ordering is haversine in SDK code. The
R\*Tree is a candidate generator, never the answer — a bounding-box prefilter
degrades at high latitudes, exactly where data is densest
([`ADR-005`](docs/adr/ADR-005-postgis-sqlite-rtree.md)).

---

## 7. Localization

```swift
country.localizedName(for: .turkish)   // "Türkiye"
country.localizedName(for: .english)   // "Turkey"
place.localizedName(for: .turkish)
place.names(for: .turkish)             // [(name, script)] — all available
```

```kotlin
country.localizedName(Locale("tr"))
place.localizedName(Locale("tr"))
```

Fallback chain from SPEC §7, identical in both SDKs:

```text
requested language → locale fallback (tr-TR → tr → default) → "en" → local name
```

Missing translations are normal, not an error. SPEC §7 says so and a package must
never fail validation because a place has one name.

A caveat to be explicit about: until `alternateNamesV2` is ingested
([`data-pipeline/sources/README.md`](data-pipeline/sources/README.md)), packages
carry a single name per place, so `localizedName` returns the English name for
every language. The API is specified; the data is not there yet.

---

## 8. Postal codes

Both directions, because SPEC §18 requires both and forbids assuming a one-to-one
relationship.

```swift
Terra.postalCode("07160")                             // PostalCode?
Terra.postalCodes(country: "TR", city: "Antalya")     // [PostalCode]
postalCode.areas                                      // [Place] — may be several
```

```kotlin
Terra.postalCode("07160")
Terra.postalCodes(country = "TR", city = "Antalya")
postalCode.areas
```

`areas` returns a list because a code can cover several places — 6 in Türkiye do
across districts, and in the US a ZIP can cover thousands of streets. Exactly one
is marked primary, chosen as the most specific one
([`DATA_MODEL.md` §8](DATA_MODEL.md#8-postal-codes)).

Countries with no postal system (IE, JP — although Japan does publish one here)
return `nil`, not an empty-code error.

---

## 9. Attribution

```swift
Terra.attribution            // [String] — ready-to-render notices
Terra.attributionString      // newline-joined
Terra.license(for: "TR")     // "CC-BY-4.0"
```

```kotlin
Terra.attribution            // List<String>
Terra.attributionString
Terra.license("TR")
```

SPEC §29 says the project must never hide source attribution. A consumer under CC
BY needs to discharge an obligation in their own UI, so the SDK hands them the
exact strings. Displaying them is the application's job, and the docs say so
rather than implying the SDK handles it.

---

## 10. Errors

Typed, and specific about which failure occurred.

```swift
public enum TerraError: Error {
    case packageNotInstalled(country: String?)
    case featureUnavailable(feature: String, packageLevel: String)
    case schemaTooNew(found: Int, supported: Int, minimumSDK: String)
    case packageCorrupted(checksumMismatch: Bool)
    case signatureInvalid(keyID: String?)
    case databaseUnreadable(underlying: String)
    case unsupportedSQLiteVersion(found: String, required: String)
}
```

```kotlin
public sealed class TerraException(message: String) : Exception(message) {
    class PackageNotInstalled(val country: String?)
    class FeatureUnavailable(val feature: String, val packageLevel: String)
    class SchemaTooNew(val found: Int, val supported: Int, val minimumSDK: String)
    class PackageCorrupted(val checksumMismatch: Boolean)
    class SignatureInvalid(val keyID: String?)
    class DatabaseUnreadable(override val cause: Throwable)
    class UnsupportedSQLiteVersion(val found: String, val required: String)
}
```

| Error | When |
|---|---|
| `packageNotInstalled` | a query needs a country whose package is absent |
| `featureUnavailable` | `searchStreet` without a `streets` package — the SDK checks `manifest.features` rather than querying a table that may not be populated |
| `schemaTooNew` | `schemaVersion` above the SDK's supported range; the error names the SDK version that is needed |
| `packageCorrupted` | post-install checksum mismatch |
| `signatureInvalid` | key not in the SDK keyring, or revoked |
| `unsupportedSQLiteVersion` | below 3.34, which the trigram tokenizer requires |

`featureUnavailable` deserves its own case. `searchStreet("X")` on a `cities`
package cannot return a useful answer, and returning an empty array would look
like "no streets match" — a silent, plausible, wrong answer.

---

## 11. Concurrency

Swift 6 and Kotlin coroutines, both read-only and both cheap to share.

```swift
// Sendable value types, opened once
let store = try Terra.open()

// Heavy work off the main actor
let results = await Terra.search("Antalya", country: "TR", limit: 20)

// Synchronous API is available and documented as blocking
let antalya = try Terra.city("Antalya")
```

```kotlin
// Suspending API
val results = Terra.search("Antalya", country = "TR", limit = 20)

// Blocking API for callers already off the main thread
val antalya = Terra.city("Antalya")
```

One `sqlite3` connection per thread, or a single connection behind a lock. SQLite
in serialized mode is fast enough at these query shapes that pooling would be
premature; the connection count stays at one per thread because that is what
`sqlite3_threadsafe` actually supports cleanly.

Every public type is `Sendable` / immutable. A mutable shared cursor is not
exposed, because it is the obvious way to introduce a data race in a read-only
library.

---

## 12. Memory

Enforced, not documented-and-hoped-for (SPEC §41).

| Rule | Enforcement |
|---|---|
| no full-table scans in any public API | queries are index-driven by construction |
| every list API takes a limit | hard default: 1000 entities, 50 autocomplete |
| keyset pagination, never `OFFSET` | no `OFFSET` in any query |
| precomputed ranks | no in-SQL ranking, so no sort-then-truncate |
| CI memory ceiling test | peak RSS during a large search is asserted |

A test that fails when a search exceeds the documented memory bound is the only
thing that keeps this from eroding during ordinary refactoring.

---

## 13. Anti-patterns

Documented because they are the obvious things to write, and each is wrong here.

**Parsing IDs.** `id.split(":")` to get the country code. IDs are opaque and their
authority values change — ISO 3166-2 codes get revised between editions, so a
parse becomes a breaking change for every consumer. Use `place.countryCode`.

**Normalizing twice.** Applying the SDK's normalizer to a value already read from
`name_norm`. Idempotent by specification, so harmless, but it obscures which code
path is under test.

**Recomputing phonetic codes.** They ship in `phonetic_codes`
([`ADR-003`](docs/adr/ADR-003-search-mechanisms.md)). Computing them in Swift,
Kotlin *and* Rust and expecting the three to agree is the liability the pipeline
exists to avoid.

**Transliterating in the SDK.** Cross-script search runs through precomputed
`place_alternate_names` rows
([`DATA_MODEL.md` §5.2](DATA_MODEL.md#52-transliteration-policy)).

**Fetching a country on demand.** The SDK does not download. `Terra.country("JP")`
on a machine with no JP package throws `packageNotInstalled`. Downloading is the
CLI's job.

**A query-time network fallback.** Even behind a feature flag. SPEC §15 makes
offline a core property, and a network branch in a query path is how that
guarantee dies.

**`SELECT *` into a struct.** Unknown columns must not become unknown SDK
properties; the SDK pins the columns it reads so a package built with a newer
schema still reads cleanly
([`PACKAGE_FORMAT.md` §13](PACKAGE_FORMAT.md#13-compatibility-rules)).

---

## 14. Conformance

`tests/integration/conformance/` runs the same query battery against the same
package bytes in the CLI, Swift and Kotlin, and compares **result ID sets and
orderings**, not just counts.

```bash
swift test --filter Conformance
./gradlew :test --tests "*Conformance*"
cargo test --package terrakit-cli conformance
```

Required in CI for any change touching normalization, search, hierarchy,
ordering or limits. A feature present in one SDK and absent in another fails,
because "equivalent functionality" is SPEC §51's requirement and equivalence is
not a thing you can assert by reading.

Cross-language normalization is additionally checked against
`tests/fixtures/normalization.json`, the shared golden vectors from
[`core/spec/normalization.md`](core/spec/normalization.md).
