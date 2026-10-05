//! Package integrity verification.
//!
//! The question this answers is narrower than "is this package trustworthy", and
//! the distinction matters. Signature verification asks *who published this*;
//! checksum verification asks *is the file on my disk still the file that was
//! published*. A local attacker with write access to `.terrakit/` passes every
//! signature check — the signature is still valid, it is just attached to
//! different bytes. That is the case this covers, and `SECURITY.md` §7 is
//! explicit that signing does not close it.
//!
//! `CLI_SPEC.md` §2.5 shows an Integrity column in `terrakit list` for exactly this
//! reason: tampering must be visible without running a separate verification
//! command.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Result of checking one file against the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Verified,
    /// Present, but its bytes do not match the recorded digest.
    Modified,
    /// The manifest lists it, but it is not on disk.
    Missing,
    /// Present, but the manifest does not list it. See `PackageFormat.md` §5.1:
    /// anything not on the allowlist is an error, not a warning.
    Unexpected,
}

#[derive(Debug, Clone)]
pub struct FileCheck {
    pub name: String,
    pub status: FileStatus,
    pub expected: String,
}

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(sha256_bytes(&bytes))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify a package directory against its manifest.
///
/// Returns the per-file results plus whether the package as a whole verifies.
/// A package with an `Unexpected` file fails: `PACKAGE_FORMAT.md` §5.1 requires
/// an allowlist, because an extra file is either a corrupted download or a
/// supply-chain attempt and both deserve the same response.
pub fn verify_package(directory: &Path) -> Result<Verification, String> {
    let manifest_path = directory.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("manifest is not valid JSON: {e}"))?;

    let files = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "manifest has no files[] array".to_string())?;

    let mut checks = Vec::new();
    for entry in files {
        let name = entry
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "manifest file entry has no path".to_string())?
            .to_string();
        let expected = entry
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let path = directory.join(&name);

        let status = if !path.is_file() {
            FileStatus::Missing
        } else if expected.is_empty() {
            // A manifest entry with no digest cannot be verified. Treated as a
            // failure rather than skipped: silently trusting an unverifiable file
            // is how a checksum requirement decays into a no-op.
            FileStatus::Unexpected
        } else {
            match sha256_file(&path) {
                Ok(actual) if actual == expected => FileStatus::Verified,
                Ok(_) => FileStatus::Modified,
                Err(_) => FileStatus::Missing,
            }
        };
        checks.push(FileCheck {
            name,
            status,
            expected,
        });
    }

    // Anything in the directory the manifest does not list.
    //
    // Two files are exempt by construction, not by special case:
    //
    // * `manifest.json` lists the data files. It cannot list itself, because
    //   listing its own digest would be self-referential.
    // * `signature.json` covers no data. Its integrity comes from the signature
    //   over the manifest bytes, which is verified separately.
    //
    // Forgetting the first one made every well-formed package report "unexpected
    // files", which is the failure mode that teaches people to ignore a check.
    const METADATA_FILES: [&str; 2] = ["manifest.json", "signature.json"];

    let listed: Vec<String> = checks.iter().map(|c| c.name.clone()).collect();
    let mut unexpected = Vec::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !listed.contains(&name) && !METADATA_FILES.contains(&name.as_str()) {
                unexpected.push(name);
            }
        }
    }
    for name in unexpected {
        checks.push(FileCheck {
            name,
            status: FileStatus::Unexpected,
            expected: String::new(),
        });
    }

    let ok = checks.iter().all(|c| c.status == FileStatus::Verified);
    Ok(Verification { ok, checks })
}

#[derive(Debug, Clone)]
pub struct Verification {
    pub ok: bool,
    pub checks: Vec<FileCheck>,
}

impl Verification {
    /// One-line summary for `list` and `doctor`.
    pub fn summary(&self) -> &'static str {
        if self.ok {
            "verified"
        } else if self
            .checks
            .iter()
            .any(|c| c.status == FileStatus::Modified)
        {
            "modified"
        } else if self
            .checks
            .iter()
            .any(|c| c.status == FileStatus::Missing)
        {
            "incomplete"
        } else {
            "unexpected files"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vector() {
        // The SHA-256 of "hello", which is the reference value in FIPS 180-2's
        // examples. Catches a mis-wired digest rather than a subtle difference.
        assert_eq!(
            sha256_bytes(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn empty_input_has_the_published_digest() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn fixture(dir: &Path, data: &[u8], digest: &str, extra: Option<&str>) {
        std::fs::write(dir.join("data.sqlite"), data).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"packageFormatVersion":1,"files":[{{"path":"data.sqlite","sha256":"{digest}"}}]}}"#
            ),
        )
        .unwrap();
        if let Some(name) = extra {
            std::fs::write(dir.join(name), b"surprise").unwrap();
        }
    }

    #[test]
    fn a_matching_package_verifies() {
        let dir = tempdir("verified");
        fixture(&dir, b"payload", &sha256_bytes(b"payload"), None);
        let result = verify_package(&dir).unwrap();
        assert!(result.ok);
        assert_eq!(result.summary(), "verified");
    }

    #[test]
    fn modified_bytes_are_detected() {
        // The case signing does not cover: a valid manifest, different bytes.
        let dir = tempdir("modified");
        fixture(&dir, b"payload", &sha256_bytes(b"original"), None);
        std::fs::write(dir.join("data.sqlite"), b"tampered").unwrap();
        let result = verify_package(&dir).unwrap();
        assert!(!result.ok);
        assert_eq!(result.summary(), "modified");
    }

    #[test]
    fn a_missing_file_is_reported_as_incomplete() {
        let dir = tempdir("missing");
        fixture(&dir, b"payload", &sha256_bytes(b"payload"), None);
        std::fs::remove_file(dir.join("data.sqlite")).unwrap();
        let result = verify_package(&dir).unwrap();
        assert!(!result.ok);
        assert_eq!(result.summary(), "incomplete");
    }

    #[test]
    fn an_unlisted_file_fails_the_package() {
        // PACKAGE_FORMAT.md 5.1: the contents are an allowlist. An extra file is
        // either a corrupted download or a supply-chain attempt.
        let dir = tempdir("extra");
        fixture(&dir, b"payload", &sha256_bytes(b"payload"), Some("payload2.exe"));
        let result = verify_package(&dir).unwrap();
        assert!(!result.ok);
        assert_eq!(result.summary(), "unexpected files");
    }

    #[test]
    fn the_manifest_does_not_flag_itself() {
        // Regression: manifest.json is not listed in files[] — it cannot be,
        // since that would be self-referential — so an allowlist that did not
        // exempt it reported every valid package as containing unexpected files.
        let dir = tempdir("self");
        fixture(&dir, b"payload", &sha256_bytes(b"payload"), None);
        let result = verify_package(&dir).unwrap();
        assert!(
            result.ok,
            "manifest.json must not be flagged: {:?}",
            result.checks
        );
    }

    #[test]
    fn signature_json_is_not_treated_as_unexpected() {
        // It carries no data digest; its integrity comes from the signature over
        // the manifest. Flagging it would make every signed package "invalid".
        let dir = tempdir("signed");
        fixture(&dir, b"payload", &sha256_bytes(b"payload"), None);
        std::fs::write(dir.join("signature.json"), b"{}").unwrap();
        assert!(verify_package(&dir).unwrap().ok);
    }

    #[test]
    fn a_manifest_entry_without_a_digest_is_not_trusted() {
        let dir = tempdir("nodigest");
        std::fs::write(dir.join("data.sqlite"), b"payload").unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"files":[{"path":"data.sqlite"}]}"#,
        )
        .unwrap();
        let result = verify_package(&dir).unwrap();
        assert!(!result.ok, "an unverifiable file must not pass silently");
    }

    fn tempdir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-verify-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
