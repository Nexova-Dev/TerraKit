//! Registry resolution: index, version selection, URL building.
//!
//! The registry is static files (`PACKAGE_FORMAT.md` §11), so this module is JSON over HTTP with
//! no state and no protocol of its own. Everything that makes a package manager hard is
//! elsewhere: verifying the bytes (`crate::signature`), fetching them (`crate::download`) and
//! putting them in place atomically (`crate::install`).
//!
//! Two transports are supported and the difference is not incidental. `https://` is the normal
//! case. `file://` exists because a package manager that cannot install from a local directory
//! cannot be tested offline, cannot be used in an air-gapped deployment, and cannot be used to
//! debug a registry problem without re-fetching everything. It is a supported feature, not a test
//! affordance — and it is still verified, so a `file://` install is exactly as safe as an
//! `https://` one.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// A package as listed in `index.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: Option<u32>,
    /// Relative to the registry root. Preferred over `manifestUrl`.
    #[serde(rename = "manifestPath", default)]
    pub manifest_path: Option<String>,
    /// Absolute. Recorded for consumers that need a full URL, but the client resolves against
    /// its own `--registry` so a mirror or an offline copy actually redirects reads.
    #[serde(rename = "manifestUrl", default)]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: u32,
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub packages: Vec<IndexEntry>,
}

/// One published version, from `packages/{name}.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: Option<u32>,
    /// Relative to the registry root, and preferred.
    #[serde(rename = "manifestPath", default)]
    pub manifest_path: Option<String>,
    /// Absolute, honoured as-is. Recorded so a registry may publish absolute URLs, but the
    /// relative form wins when both are present — otherwise a mirror or an offline copy of the
    /// tree would keep reading the origin.
    #[serde(rename = "manifestUrl", default)]
    pub manifest_url: Option<String>,
    #[serde(rename = "downloadUrl", default)]
    pub download_url: Option<String>,
    #[serde(rename = "downloadPath", default)]
    pub download_path: Option<String>,
    /// Digest of the artifact as downloaded, before decompression.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Digest of the compressed artifact, checked before decompression starts.
    #[serde(rename = "compressedSha256", default)]
    pub compressed_sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackageIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub latest: Option<VersionEntry>,
    #[serde(default)]
    pub versions: Vec<VersionEntry>,
}

/// `+` in a dataset version becomes `-` in a URL path segment.
///
/// Must match `tools/build_registry.py`. Recorded as a constant rather than duplicated because
/// a mismatch makes every download 404 with an error that says nothing about the cause — the
/// most expensive kind of bug to diagnose, since everything upstream looks correct.
pub const VERSION_PATH_SEPARATOR: char = '-';

pub fn version_to_path_segment(version: &str) -> String {
    version.replace('+', &VERSION_PATH_SEPARATOR.to_string())
}

/// Join a registry base URL with a path, tolerating a missing or duplicated trailing slash.
pub fn url_join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Resolve a path a registry served, relative to the registry the client was pointed at.
///
/// An absolute path or URL is returned unchanged, so a registry may still publish absolute URLs.
/// A relative one is resolved against `registry` rather than against anything embedded in the
/// index — otherwise a copied tree, a mirror or an offline copy would keep reading the origin and
/// "point at your mirror" would silently do nothing.
pub fn resolve(registry: &str, candidate: &str) -> String {
    let candidate = candidate.trim();
    if candidate.starts_with("http://")
        || candidate.starts_with("https://")
        || candidate.starts_with("file://")
    {
        return candidate.to_string();
    }
    url_join(registry, candidate)
}

/// The URL of a version's manifest, from whichever form the registry published.
pub fn manifest_for(registry: &str, package: &str, entry: &VersionEntry) -> String {
    entry
        .manifest_path
        .as_deref()
        .or(entry.manifest_url.as_deref())
        .map(|path| resolve(registry, path))
        .unwrap_or_else(|| {
            manifest_url(registry, package, &entry.version)
        })
}

/// The URL of a version's download artifact.
pub fn download_for(registry: &str, package: &str, entry: &VersionEntry) -> Option<String> {
    entry
        .download_path
        .as_deref()
        .or(entry.download_url.as_deref())
        .map(|path| resolve(registry, path))
        .or_else(|| {
            entry.manifest_url.as_deref().map(|_| {
                let base = manifest_url(registry, package, &entry.version);
                format!(
                    "{}/{}",
                    base.rsplit_once('/').map(|(b, _)| b).unwrap_or(&base),
                    "data.sqlite.zst"
                )
            })
        })
}

/// The URL of a version's signature, next to its manifest.
pub fn signature_for(manifest_url: &str) -> String {
    format!(
        "{}/signature.json",
        manifest_url.rsplit_once('/').map(|(base, _)| base).unwrap_or(manifest_url)
    )
}

/// The registry index.
pub fn index_url(registry: &str) -> String {
    url_join(registry, "index.json")
}

/// The per-package version list.
pub fn package_url(registry: &str, package: &str) -> String {
    url_join(registry, &format!("packages/{package}.json"))
}

/// The manifest for one version.
pub fn manifest_url(registry: &str, package: &str, version: &str) -> String {
    url_join(
        registry,
        &format!(
            "packages/{package}/{}/manifest.json",
            version_to_path_segment(version)
        ),
    )
}

// --- version ranges -------------------------------------------------------

/// A version, split for comparison.
///
/// The `+<hash>` build suffix is ignored, following semver: `2026.10.1+a6c3` and `2026.10.1+e83e`
/// are the same release line and either satisfies `^2026.10.0`. The hash identifies which
/// upstream extract a build came from, not a different version of the format, and treating it as
/// a version component would make `^2026.10.0` stop matching every rebuild.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub parts: Vec<u64>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let main = text.split('+').next().unwrap_or(text);
        if main.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for segment in main.split('.') {
            parts.push(segment.parse::<u64>().ok()?);
        }
        Some(Version { parts })
    }

    fn at(&self, index: usize) -> u64 {
        self.parts.get(index).copied().unwrap_or(0)
    }
}

/// A version range, the subset of Cargo/semver syntax that makes sense for a CalVer dataset.
///
/// Supported: `*`, `latest`, an exact version, `^`, `~`, `>=`, and `=`. Nothing else — an
/// unrecognised range is an error naming what it saw, never a silent fallback to "the newest
/// version", which would install something the user did not ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRange {
    /// Anything.
    Any,
    Exact(Version),
    /// `^1.2.3`: compatible-with, so `>=1.2.3` and `<2.0.0` for the first non-zero component.
    Caret(Version),
    /// `~1.2.3`: `>=1.2.3` and `<1.3.0`.
    Tilde(Version),
    /// `>=1.2.3`.
    AtLeast(Version),
}

impl VersionRange {
    pub fn parse(text: &str) -> Result<VersionRange, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed == "*" || trimmed.eq_ignore_ascii_case("latest") {
            return Ok(VersionRange::Any);
        }
        let (operator, rest) = if let Some(rest) = trimmed.strip_prefix('^') {
            ("^", rest)
        } else if let Some(rest) = trimmed.strip_prefix('~') {
            ("~", rest)
        } else if let Some(rest) = trimmed.strip_prefix(">=") {
            (">=", rest)
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            ("=", rest)
        } else {
            ("=", trimmed)
        };

        let version = Version::parse(rest.trim())
            .ok_or_else(|| format!("{text:?} is not a version; expected something like 2026.10.1"))?;

        Ok(match operator {
            "^" => VersionRange::Caret(version),
            "~" => VersionRange::Tilde(version),
            ">=" => VersionRange::AtLeast(version),
            _ => VersionRange::Exact(version),
        })
    }

    /// Whether `candidate` satisfies this range.
    pub fn matches(&self, candidate: &str) -> bool {
        let Some(version) = Version::parse(candidate) else {
            return false;
        };
        match self {
            VersionRange::Any => true,
            VersionRange::Exact(wanted) => version.parts.starts_with(&wanted.parts),
            VersionRange::AtLeast(bound) => version.parts >= bound.parts,
            VersionRange::Caret(bound) => {
                if version.parts < bound.parts {
                    return false;
                }
                // The first non-zero component of the bound is where compatibility stops.
                // `^0.10.1` allows `0.10.x` but not `0.11.0`, which is the whole point of
                // honouring the zero component rather than assuming 1.0.0.
                let mut pivot = 0;
                while pivot < bound.parts.len() && bound.at(pivot) == 0 {
                    pivot += 1;
                }
                version.at(pivot) == bound.at(pivot)
            }
            VersionRange::Tilde(bound) => {
                if version.parts < bound.parts {
                    return false;
                }
                // `~2026.10.1` allows `2026.10.9` but not `2026.11.0`.
                let pivot = bound.parts.len().saturating_sub(2).max(0);
                version.at(pivot) == bound.at(pivot)
            }
        }
    }
}

/// Highest version satisfying `range`.
///
/// Returns the requested range in the error so a failed pin says what was wanted, not just that
/// nothing matched — otherwise `terrakit add TR` on a typo'd pin reports "no matching version"
/// and the user has to guess which part was wrong.
pub fn select_version<'a>(
    versions: &'a [VersionEntry],
    range: &VersionRange,
) -> Result<&'a VersionEntry, String> {
    let matching: Vec<&VersionEntry> = versions
        .iter()
        .filter(|entry| range.matches(&entry.version))
        .collect();

    if matching.is_empty() {
        let available: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
        return Err(format!(
            "no published version satisfies this range.\n       available: {}\n\
             Check terrakit.json for the pinned version.",
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        ));
    }

    matching
        .into_iter()
        .max_by(|a, b| Version::parse(&a.version).cmp(&Version::parse(&b.version)))
        .ok_or_else(|| "no published versions".to_string())
}

/// Fetch and parse the registry index.
pub fn fetch_index(
    registry: &str,
    offline: bool,
) -> Result<Index, String> {
    let url = index_url(registry);
    let body = crate::download::fetch_text(&url, offline)?;
    serde_json::from_str(&body).map_err(|e| format!("{url} is not a valid index: {e}"))
}

/// Fetch and parse one package's version list.
pub fn fetch_package(
    registry: &str,
    package: &str,
    offline: bool,
) -> Result<PackageIndex, String> {
    let url = package_url(registry, package);
    let body = crate::download::fetch_text(&url, offline)?;
    serde_json::from_str(&body).map_err(|e| format!("{url} is not a valid package index: {e}"))
}

/// The package name for an ISO 3166-1 alpha-2 code.
///
/// `terrakit add TR`, not `terrakit add terrakit-data-tr`: the user thinks in countries, and the
/// naming convention is the registry's concern. The mapping is validated in both directions so a
/// typo is rejected before a request goes out.
pub fn package_name_for(country: &str) -> Result<String, String> {
    let code = country.trim().to_uppercase();
    if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(format!(
            "{country:?} is not an ISO 3166-1 alpha-2 code. Expected two letters, e.g. TR."
        ));
    }
    Ok(format!("terrakit-data-{}", code.to_lowercase()))
}

/// Validate an ISO code and return it uppercased.
pub fn country_code(country: &str) -> Result<String, String> {
    let code = country.trim().to_uppercase();
    if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(format!(
            "{country:?} is not an ISO 3166-1 alpha-2 code. Expected two letters, e.g. TR."
        ));
    }
    Ok(code)
}

/// Whether a local path looks like a registry tree, for `file://` installs.
///
/// Used to turn a bare directory into a registry URL rather than failing on the scheme.
pub fn registry_from_directory(path: &Path) -> Option<String> {
    if path.join("index.json").is_file() {
        Some(format!("file://{}", path.display()))
    } else {
        None
    }
}

/// Group a resolved set of packages by country, for output.
pub fn by_country(index: &Index) -> BTreeMap<String, &IndexEntry> {
    index
        .packages
        .iter()
        .filter_map(|entry| {
            entry
                .country
                .as_ref()
                .map(|country| (country.to_uppercase(), entry))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(version: &str) -> VersionEntry {
        VersionEntry {
            version: version.to_string(),
            level: None,
            schema_version: Some(1),
            manifest_url: None,
            manifest_path: None,
            download_url: None,
            download_path: None,
            sha256: None,
            compressed_sha256: None,
        }
    }

    #[test]
    fn a_relative_registry_path_is_resolved_against_the_client_registry() {
        // The property that makes mirrors and offline copies work. A registry tree copied
        // elsewhere and pointed at with --registry must be read from the copy.
        assert_eq!(
            resolve("file:///mirror", "packages/a/1/manifest.json"),
            "file:///mirror/packages/a/1/manifest.json"
        );
        assert_eq!(
            resolve("https://mirror.dev/", "packages/a/1/manifest.json"),
            "https://mirror.dev/packages/a/1/manifest.json"
        );
        // An absolute URL is honoured, so a registry may still publish them.
        assert_eq!(
            resolve("https://mirror.dev", "https://origin.dev/x.json"),
            "https://origin.dev/x.json"
        );
    }

    #[test]
    fn manifest_and_download_urls_prefer_the_relative_form() {
        let mut entry = entry("2026.10.1");
        entry.manifest_path = Some("packages/p/1/manifest.json".into());
        entry.manifest_url = Some("https://origin.dev/packages/p/1/manifest.json".into());
        assert_eq!(
            manifest_for("file:///mirror", "p", &entry),
            "file:///mirror/packages/p/1/manifest.json",
            "the relative path must win, or a mirror is never used"
        );
        assert_eq!(
            signature_for("file:///mirror/packages/p/1/manifest.json"),
            "file:///mirror/packages/p/1/signature.json"
        );
    }

    #[test]
    fn a_registry_publishing_only_absolute_urls_still_works() {
        let mut entry = entry("2026.10.1");
        entry.manifest_url = Some("https://origin.dev/packages/p/1/manifest.json".into());
        assert_eq!(
            manifest_for("file:///mirror", "p", &entry),
            "https://origin.dev/packages/p/1/manifest.json"
        );
    }

    #[test]
    fn urls_tolerate_slashes_on_either_side() {
        assert_eq!(index_url("https://x.dev"), "https://x.dev/index.json");
        assert_eq!(index_url("https://x.dev/"), "https://x.dev/index.json");
        assert_eq!(index_url("https://x.dev///"), "https://x.dev/index.json");
        assert_eq!(
            package_url("https://x.dev", "terrakit-data-tr"),
            "https://x.dev/packages/terrakit-data-tr.json"
        );
        assert_eq!(
            manifest_url("https://x.dev", "terrakit-data-tr", "2026.10.1+a6c3"),
            "https://x.dev/packages/terrakit-data-tr/2026.10.1-a6c3/manifest.json"
        );
    }

    #[test]
    fn version_segments_match_the_registry_publisher() {
        // tools/build_registry.py uses '-' where the version has '+'.
        assert_eq!(version_to_path_segment("2026.10.1+a6c3"), "2026.10.1-a6c3");
        assert_eq!(version_to_path_segment("2026.10.1"), "2026.10.1");
    }

    #[test]
    fn caret_stops_at_the_first_non_zero_component() {
        let range = VersionRange::parse("^2026.10.1").unwrap();
        assert!(range.matches("2026.10.1"));
        assert!(range.matches("2026.10.9"));
        assert!(range.matches("2026.99.0"), "any later release of the same year");
        // `^2026.10.1` is `>=2026.10.1 <2027.0.0`, so a new year is NOT allowed. Asserting the
        // opposite was my error: caret pins the major, it does not mean "any newer version".
        assert!(!range.matches("2027.1.0"), "caret stops at the next major");
        assert!(!range.matches("2026.9.0"));
        assert!(!range.matches("2025.10.1"));

        // The zero component is what makes ^0.x restrictive, and getting that wrong is how a
        // `^0.1.0` silently starts pulling breaking changes.
        let zero = VersionRange::parse("^0.10.1").unwrap();
        assert!(zero.matches("0.10.7"));
        assert!(!zero.matches("0.11.0"), "with a 0 major, the minor becomes the pivot");
        assert!(!zero.matches("1.0.0"));

        // `^0.0.3` narrows further still: all three components are significant.
        let narrow = VersionRange::parse("^0.0.3").unwrap();
        assert!(narrow.matches("0.0.3"));
        assert!(!narrow.matches("0.0.4"));
    }

    #[test]
    fn tilde_holds_the_minor_component() {
        let range = VersionRange::parse("~2026.10.1").unwrap();
        assert!(range.matches("2026.10.9"));
        assert!(!range.matches("2026.11.0"));
        assert!(!range.matches("2026.9.9"));
    }

    #[test]
    fn a_build_suffix_does_not_break_a_pinned_range() {
        // The package is 2026.10.1+a6c3. A pin of 2026.10.1 must still match it, or every
        // published package becomes uninstallable by its own version.
        let exact = VersionRange::parse("2026.10.1").unwrap();
        assert!(exact.matches("2026.10.1+a6c3"));
        assert!(exact.matches("2026.10.1+e83e"));

        let caret = VersionRange::parse("^2026.10.0").unwrap();
        assert!(caret.matches("2026.10.1+a6c3"));
    }

    #[test]
    fn unknown_ranges_are_rejected_rather_than_defaulting_to_latest() {
        // Silently installing the newest version when a pin was misspelled is the worst
        // possible response: it appears to succeed and installs something unrequested.
        for bad in [
            "2026.10.x",
            "v2026.10.1",
            "=>1.0",
            "2026-10-01",
            "latest..1.0",
            "next",
        ] {
            assert!(
                VersionRange::parse(bad).is_err(),
                "{bad:?} must be rejected, not treated as 'any'"
            );
        }
    }

    #[test]
    fn the_unqualified_forms_mean_any_version() {
        for any in ["", "*", "latest", "LATEST", "  *  "] {
            assert_eq!(
                VersionRange::parse(any).unwrap(),
                VersionRange::Any,
                "{any:?} should mean any version"
            );
        }
    }

    #[test]
    fn selection_takes_the_highest_not_the_last_listed() {
        // The registry sorts descending, but relying on that couples the client's correctness to
        // the server's ordering.
        let versions = vec![
            entry("2026.10.9"),
            entry("2026.9.1"),
            entry("2026.10.10"),
        ];
        let range = VersionRange::parse("^2026.10.0").unwrap();
        let chosen = select_version(&versions, &range).unwrap();
        assert_eq!(
            chosen.version, "2026.10.10",
            "numeric ordering, not lexicographic: 2026.10.10 > 2026.10.9"
        );
    }

    #[test]
    fn an_unsatisfiable_pin_names_the_available_versions() {
        let versions = vec![entry("2026.10.1"), entry("2026.9.1")];
        let range = VersionRange::parse("^2027.0.0").unwrap();
        let error = select_version(&versions, &range).unwrap_err();
        assert!(error.contains("2026.10.1"), "must list what is available: {error}");
        assert!(error.contains("terrakit.json"), "must say where the pin lives: {error}");
    }

    #[test]
    fn country_codes_are_validated_before_a_request_is_made() {
        assert_eq!(country_code("tr").unwrap(), "TR");
        assert_eq!(package_name_for("tr").unwrap(), "terrakit-data-tr");
        assert_eq!(package_name_for("DE").unwrap(), "terrakit-data-de");
        for bad in ["T", "TUR", "T1", "", "12"] {
            assert!(country_code(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_local_directory_is_usable_as_a_registry() {
        let dir = std::env::temp_dir().join("terrakit-registry-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), b"{}").unwrap();
        assert!(registry_from_directory(&dir).is_some());

        let empty = std::env::temp_dir().join("terrakit-registry-empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(registry_from_directory(&empty).is_none());
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&empty).ok();
    }
}