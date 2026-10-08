/// Package discovery and resolution, `SDK_SPEC.md` §2.
///
/// Resolution order, first match wins, resolved once and cached:
///
/// ```text
/// 1. explicit configuration        Terra.configure(packages: [...])
/// 2. the application bundle        iOS/macOS resource, Android assets
/// 3. .terrakit/packages/…          local development
/// ```
///
/// Step 2 is what makes `import TerraKit` work in a shipped app, since an app reads only from its
/// bundle. `terrakit bundle --platform ios`
/// ([`ARCHITECTURE.md` §6](../../ARCHITECTURE.md#6-mobile-packaging)) generates the resource
/// manifest for it.
///
/// A package that is absent produces `.packageNotInstalled`, not an empty result. Silently
/// returning nothing for an uninstalled country is the failure mode that ships a bug nobody
/// notices until a customer reports an empty dropdown.

import Foundation

/// One installed package: its database plus what its manifest says about it.
public final class Package: @unchecked Sendable {
    public let database: Database
    public let countryCode: String
    public let directory: URL

    /// `schemaVersion` from the manifest.
    public let schemaVersion: Int
    /// The dataset's level: `basic`, `administrative`, `cities`, `postal`, `full`.
    public let level: String
    /// The dataset version, e.g. `2026.10.1+9920`.
    public let version: String
    /// Declared features, e.g. `["autocomplete", "streets"]`.
    ///
    /// Checked rather than inferred from a table existing: `searchStreet` on a `cities` package
    /// cannot return a useful answer, and an empty array would look like "no streets match".
    public let features: Set<String>
    /// Licence, e.g. `CC-BY-4.0`.
    public let license: String
    /// Ready-to-render attribution notices.
    ///
    /// Spec §29 says the project must never hide source attribution. A consumer under CC BY has to
    /// discharge that obligation in their own UI, so the SDK hands over the exact strings.
    public let attribution: [String]

    /// The schema version this SDK supports.
    ///
    /// A package built with a newer schema still reads correctly for the columns the SDK pins
    /// ([`PACKAGE_FORMAT.md` §13](../../PACKAGE_FORMAT.md#13-compatibility-rules)), so this is only
    /// refused when the package declares a `minReaderVersion` above what the SDK reports.
    public static let supportedSchemaVersion = 1

    init(database: Database, directory: URL, manifest: [String: Any]) throws {
        self.database = database
        self.directory = directory
        self.countryCode = database.countryCode

        guard let schema = manifest["schemaVersion"] as? Int else {
            throw TerraError.packageCorrupted(checksumMismatch: false)
        }
        self.schemaVersion = schema

        if let minReader = manifest["minReaderVersion"] as? Int,
           minReader > Terra.sdkVersionNumber {
            throw TerraError.schemaTooNew(
                found: schema,
                supported: Package.supportedSchemaVersion,
                minimumSDK: Terra.sdkVersion
            )
        }

        self.level = manifest["level"] as? String ?? "unknown"
        self.version = manifest["version"] as? String ?? "unknown"
        self.features = Set(manifest["features"] as? [String] ?? [])
        self.license = manifest["license"] as? String ?? "unknown"
        self.attribution = manifest["attribution"] as? [String] ?? []
    }

    /// Whether this package provides a declared feature.
    public func hasFeature(_ feature: String) throws {
        guard features.contains(feature) else {
            throw TerraError.featureUnavailable(feature: feature, packageLevel: level)
        }
    }
}

/// Finds packages and holds the resolved set.
public final class PackageResolver: @unchecked Sendable {
    /// Roots to search, in order. First match wins.
    public let roots: [URL]

    private let lock = NSLock()
    private var cache: [String: Package] = [:]

    /// Key ids the caller trusts, for signature checking by a consumer that wants it.
    ///
    /// The SDK does not verify signatures itself — `terrakit add` does, before the package is ever
    /// written to disk ([`SECURITY.md` §3](../../SECURITY.md)). This exists so an app that bundles
    /// packages itself can check the same keyring rather than trusting the file's presence.
    public var trustedKeyIDs: Set<String> = []

    public init(roots: [URL]) {
        self.roots = roots
    }

    /// The default search roots, in SDK_SPEC §2 order.
    public static func defaultRoots(bundle: Bundle = .main, workingDirectory: URL = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)) -> [URL] {
        var roots: [URL] = []

        // 2. the application bundle
        for name in ["TerraKit", "terrakit"] {
            if let resource = bundle.url(forResource: name, withExtension: nil) {
                roots.append(resource)
            }
        }

        // 3. .terrakit/packages, walking up from the working directory
        var current = workingDirectory.standardizedFileURL
        for _ in 0..<8 {
            let candidate = current.appendingPathComponent(".terrakit/packages")
            if FileManager.default.fileExists(atPath: candidate.path) {
                roots.append(candidate)
            }
            let parent = current.deletingLastPathComponent()
            if parent.path == current.path { break }
            current = parent
        }

        return roots
    }

    /// One candidate package directory, with the country and version it declares.
    private struct Candidate {
        let country: String
        let version: String
        let directory: URL
    }

    /// Every candidate package across every root, best first per country.
    ///
    /// Two layouts are accepted, because both exist in the wild:
    ///
    /// ```text
    /// canonical   <root>/TR/2026.10.1-9920/data.sqlite     <- .terrakit/packages
    /// flat        <root>/terrakit-data-tr-2026.10.1+9920/data.sqlite   <- a dataset directory
    /// ```
    ///
    /// The canonical one is what `terrakit add` writes and what a shipped app's bundle contains.
    /// The flat one is what a person has after `terrakit-pipeline build --out datasets`, and
    /// pointing the SDK at that directory is a reasonable thing to want — it is how the conformance
    /// suite runs without laying out a project first.
    private func candidates() -> [Candidate] {
        var found: [Candidate] = []
        let manager = FileManager.default

        for root in roots {
            guard let entries = try? manager.contentsOfDirectory(
                atPath: root.path
            ) else { continue }

            for entry in entries {
                let path = root.appendingPathComponent(entry)

                // Flat: terrakit-data-<cc>-<version>
                if entry.hasPrefix("terrakit-data-") {
                    let parts = entry.split(separator: "-", maxSplits: 3, omittingEmptySubsequences: false)
                    guard parts.count == 4,
                          parts[0] == "terrakit",
                          parts[1] == "data"
                    else { continue }
                    let country = String(parts[2]).uppercased()
                    let version = String(parts[3])
                    guard country.count == 2, country.allSatisfy({ $0.isLetter }) else { continue }
                    guard manager.fileExists(
                        atPath: path.appendingPathComponent("data.sqlite").path
                    ) else { continue }
                    found.append(Candidate(country: country, version: version, directory: path))
                    continue
                }

                // Canonical: <CC>/<version>
                guard entry.count == 2,
                      entry == entry.uppercased(),
                      entry.allSatisfy({ $0.isLetter })
                else { continue }
                guard let versions = try? manager.contentsOfDirectory(atPath: path.path) else { continue }
                for version in versions {
                    let directory = path.appendingPathComponent(version)
                    guard manager.fileExists(
                        atPath: directory.appendingPathComponent("data.sqlite").path
                    ) else { continue }
                    found.append(
                        Candidate(country: entry, version: version, directory: directory)
                    )
                }
            }
        }
        return found
    }

    /// Every installed country, in a stable order.
    ///
    /// Sorted by ISO code so the result is deterministic: an unsorted directory listing makes a
    /// conformance test compare two different orders and report a spurious difference.
    public func installedCountries() -> [String] {
        Set(candidates().map(\.country)).sorted()
    }

    /// A comparable key for a package directory's declared version.
    ///
    /// Numeric, so `2026.10.10` beats `2026.10.9`. The `+suffix` is ignored, following semver:
    /// `2026.10.1+a6c3` and `2026.10.1+e83e` are the same release line.
    static func versionKey(of directory: URL) -> [Int] {
        guard let data = try? Data(contentsOf: directory.appendingPathComponent("manifest.json")),
              let manifest = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let version = manifest["version"] as? String
        else {
            // An unreadable manifest sorts first, so a directory that at least parses wins over
            // one that does not.
            return [0]
        }
        let main = version.split(separator: "+").first.map(String.init) ?? version
        return main.split(separator: ".").map { Int($0) ?? 0 }
    }

    /// The package for a country, or `nil` when it is absent.
    ///
    /// Resolves the **newest** version when several are present. `terrakit update` removes the
    /// version it supersedes, so more than one means a rollback happened and the newest is the one
    /// a user means.
    ///
    /// Ordering is by the version each manifest declares, not by directory name: directory names
    /// are the version with `+` replaced by `-`, and string ordering puts `2026.10.10` before
    /// `2026.10.9`, so "newest" would be whichever one a lexical sort happens to put last.
    public func package(for countryCode: String) throws -> Package? {
        let code = countryCode.uppercased()

        lock.lock()
        if let cached = cache[code] {
            lock.unlock()
            return cached
        }
        lock.unlock()

        let matching = candidates()
            .filter { $0.country == code }
            .sorted { lhs, rhs in
                let a = PackageResolver.versionKey(of: lhs.directory)
                let b = PackageResolver.versionKey(of: rhs.directory)
                // Element-wise and padded: [2026, 10] and [2026, 10, 1] must compare equal rather
                // than by length, so a two-component version does not sort above a three-component
                // one.
                for index in 0..<max(a.count, b.count) {
                    let left = index < a.count ? a[index] : 0
                    let right = index < b.count ? b[index] : 0
                    if left != right { return left < right }
                }
                return false
            }

        for candidate in matching.reversed() {
            let manifestURL = candidate.directory.appendingPathComponent("manifest.json")
            guard let manifestData = try? Data(contentsOf: manifestURL),
                  let manifest = try? JSONSerialization.jsonObject(with: manifestData) as? [String: Any]
            else { continue }

            do {
                let database = try Database(
                    url: candidate.directory.appendingPathComponent("data.sqlite"),
                    countryCode: code
                )
                let package = try Package(database: database, directory: candidate.directory, manifest: manifest)
                lock.lock()
                cache[code] = package
                lock.unlock()
                return package
            } catch {
                // A damaged or unreadable package is skipped in favour of another version. One bad
                // package must not make every other country unqueryable.
                continue
            }
        }
        return nil
    }

    /// The package for a country, or `.packageNotInstalled`.
    ///
    /// The single place that turns absence into a typed error, so every call site cannot forget.
    public func requirePackage(_ countryCode: String?) throws -> Package {
        if let countryCode {
            guard let package = try package(for: countryCode) else {
                throw TerraError.packageNotInstalled(country: countryCode.uppercased())
            }
            return package
        }
        throw TerraError.packageNotInstalled(country: nil)
    }

    /// Every opened package, in country order.
    public func allPackages() throws -> [Package] {
        try installedCountries().compactMap { try package(for: $0) }
    }
}