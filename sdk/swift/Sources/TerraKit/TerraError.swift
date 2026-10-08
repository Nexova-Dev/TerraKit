/// Typed errors, `SDK_SPEC.md` §10.
///
/// Typed and specific about which failure occurred. A missing package and an empty result are
/// different outcomes and must not look alike: silently returning nothing for an uninstalled
/// country is the failure mode that ships a bug nobody notices until a customer reports an empty
/// dropdown.
public enum TerraError: Error, Sendable, Equatable {
    /// A query needs a country whose package is absent.
    case packageNotInstalled(country: String?)

    /// The requested feature is not in this package — `searchStreet` without a `streets` package.
    ///
    /// Its own case because returning an empty array would look like "no streets match": a
    /// silent, plausible, wrong answer. The SDK checks `manifest.features` rather than querying a
    /// table that may not be populated.
    case featureUnavailable(feature: String, packageLevel: String)

    /// `schemaVersion` above the SDK's supported range.
    case schemaTooNew(found: Int, supported: Int, minimumSDK: String)

    /// Post-install checksum mismatch.
    case packageCorrupted(checksumMismatch: Bool)

    /// Signature key not in the SDK keyring, or revoked.
    case signatureInvalid(keyID: String?)

    /// The database could not be opened or read.
    case databaseUnreadable(underlying: String)

    /// Below the SQLite version the trigram tokenizer requires.
    case unsupportedSQLiteVersion(found: String, required: String)
}

extension TerraError: CustomStringConvertible {
    public var description: String {
        switch self {
        case .packageNotInstalled(let country):
            guard let country else {
                return "no TerraKit packages are installed"
            }
            return """
                the \(country) package is not installed. Run `terrakit add \(country)`. \
                The SDK does not download: `Terra.country("\(country)")` on a machine without the \
                package throws this rather than returning an empty result.
                """
        case .featureUnavailable(let feature, let level):
            return """
                \(feature) is unavailable: this package is at level '\(level)'. \
                Install a package that provides it.
                """
        case .schemaTooNew(let found, let supported, let minimumSDK):
            return """
                this package uses schema version \(found), and this SDK supports \(supported). \
                TerraKit SDK \(minimumSDK) or newer is required.
                """
        case .packageCorrupted(let checksumMismatch):
            return checksumMismatch
                ? "the installed package does not match its manifest checksum. Reinstall it."
                : "the installed package is damaged. Reinstall it."
        case .signatureInvalid(let keyID):
            guard let keyID else {
                return "the package is unsigned and unsigned packages are refused"
            }
            return """
                the package was signed by key '\(keyID)', which is not in the SDK keyring. \
                Add it to your keyring through `Terra.configure(trustedKeyIDs:)` before installing.
                """
        case .databaseUnreadable(let underlying):
            return "the package database could not be read: \(underlying)"
        case .unsupportedSQLiteVersion(let found, let required):
            return """
                SQLite \(found) is too old; \(required) or newer is required for the trigram \
                tokenizer the search index uses.
                """
        }
    }
}