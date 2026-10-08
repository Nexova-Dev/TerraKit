package com.terrakit.core

/**
 * Typed errors, `SDK_SPEC.md` §10.
 *
 * Typed and specific about which failure occurred. A missing package and an empty result are
 * different outcomes and must not look alike: silently returning nothing for an uninstalled country
 * is the failure mode that ships a bug nobody notices until a customer reports an empty dropdown.
 */
sealed class TerraError(message: String) : Exception(message) {

    /** A query needs a country whose package is absent. */
    class PackageNotInstalled(val country: String?) :
        TerraError(
            if (country == null) {
                "no TerraKit packages are installed"
            } else {
                "the $country package is not installed. Run `terrakit add $country`. " +
                    "The SDK does not download: Terra.country(\"$country\") on a machine without the " +
                    "package throws this rather than returning an empty result."
            }
        )

    /**
     * The requested feature is not in this package — `searchStreet` without a `streets` package.
     *
     * Its own case because returning an empty list would look like "no streets match": a silent,
     * plausible, wrong answer. The SDK checks the manifest's declared features rather than querying
     * a table that may not be populated.
     */
    class FeatureUnavailable(val feature: String, val packageLevel: String) : TerraError(
        "$feature is unavailable: this package is at level '$packageLevel'. " +
            "Install a package that provides it."
    )

    /** `schemaVersion` above the SDK's supported range. */
    class SchemaTooNew(val found: Int, val supported: Int, val minimumSdk: String) : TerraError(
        "this package uses schema version $found, and this SDK supports $supported. " +
            "TerraKit SDK $minimumSdk or newer is required."
    )

    /** Post-install checksum mismatch, or damage of another kind. */
    class PackageCorrupted(val checksumMismatch: Boolean) : TerraError(
        if (checksumMismatch) {
            "the installed package does not match its manifest checksum. Reinstall it."
        } else {
            "the installed package is damaged. Reinstall it."
        }
    )

    /** Signature key not in the SDK keyring, or revoked. */
    class SignatureInvalid(val keyId: String?) : TerraError(
        if (keyId == null) {
            "the package is unsigned and unsigned packages are refused"
        } else {
            "the package was signed by key '$keyId', which is not in the SDK keyring. " +
                "Add it to your keyring through Terra.configure(trustedKeyIds) before installing."
        }
    )

    /** The database could not be opened or read. */
    class DatabaseUnreadable(val detail: String) :
        TerraError("the package database could not be read: $detail")

    /** Below the SQLite version the trigram tokenizer requires. */
    class UnsupportedSqliteVersion(val found: String, val required: String) : TerraError(
        "SQLite $found is too old; $required or newer is required for the trigram tokenizer " +
            "the search index uses."
    )
}