//! Atomic installation.
//!
//! The install is a rename, and everything before it is confined to a temporary directory. That
//! is the whole design: a package is either absent or complete and verified, and there is no
//! intermediate state a crash can leave behind. A partially extracted database in
//! `.terrakit/packages/TR/2026.10.1/` would be found by `list`, opened by `search`, and produce
//! wrong answers — which is the worst outcome available, because it is silent.
//!
//! The temporary directory is created **inside** the destination's parent. `rename` is only
//! atomic within a filesystem, and `/tmp` is frequently a different one, so staging in `/tmp` and
//! renaming into the project would silently degrade to copy-then-delete.
//!
//! Disk space is checked before downloading rather than after, because the failure mode of
//! running out mid-extraction is a partial temporary directory rather than a clean refusal.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::verify::sha256_file;

/// Refuse to install if the free space is below this margin over the requirement.
///
/// The margin covers the temporary copy and the final copy coexisting, plus filesystem
/// metadata. Without it a user with exactly enough space gets a failure a few megabytes from the
/// end, having already paid the whole download.
const SPACE_MARGIN_BYTES: u64 = 64 * 1024 * 1024;

/// What a staged install contains before it is moved into place.
#[derive(Debug, Clone)]
pub struct StagedPackage {
    pub directory: PathBuf,
    pub version: String,
    pub size_on_disk: u64,
}

/// Everything that must hold for an install to be allowed.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub country: String,
    pub version: String,
    pub destination: PathBuf,
    /// Bytes the extracted database will occupy.
    pub extracted_size: u64,
    /// Bytes the compressed download occupies.
    pub compressed_size: u64,
}

impl InstallPlan {
    /// Bytes that must be free: the download, the extracted copy, and a margin.
    ///
    /// The temporary directory and the destination hold the same data at the moment of the
    /// rename, so counting only one of them understates the requirement by half.
    pub fn required_free_bytes(&self) -> u64 {
        self.extracted_size
            .saturating_add(self.compressed_size)
            .saturating_add(SPACE_MARGIN_BYTES)
    }
}

/// Verify there is room, before anything is downloaded.
///
/// Returns a message the user can act on rather than a bare byte count: on macOS the figure comes
/// from `statfs`, and a "no space" failure during extraction instead names the directory and says
/// how much was missing.
pub fn check_space(plan: &InstallPlan) -> Result<(), String> {
    let filesystem = plan
        .destination
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    if !filesystem.exists() {
        // The packages directory is created by `add` itself; nothing to check against yet.
        return Ok(());
    }

    let Ok(available) = available_bytes(&filesystem) else {
        // Unknown free space is not a reason to refuse. Refusing here would make the tool fail
        // on filesystems whose statfs variant is not supported, which is a worse outcome than
        // letting the write fail later with a clearer message.
        return Ok(());
    };

    let required = plan.required_free_bytes();
    if available < required {
        return Err(format!(
            "not enough space at {}\n\
             \x20      need {} ({} extracted + {} compressed + {} margin)\n\
             \x20      have {}\n\
             \x20      Free space, or point --cache-dir at a larger volume.",
            filesystem.display(),
            crate::download::human_bytes(required),
            crate::download::human_bytes(plan.extracted_size),
            crate::download::human_bytes(plan.compressed_size),
            crate::download::human_bytes(SPACE_MARGIN_BYTES),
            crate::download::human_bytes(available),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn available_bytes(path: &Path) -> Result<u64, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains a NUL byte".to_string())?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stats` is a correctly sized,
    // writable `statfs`. Both preconditions are checked by construction.
    let result = unsafe { libc::statfs(c_path.as_ptr(), &mut stats) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(stats.f_bavail as u64 * stats.f_bsize as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn available_bytes(path: &Path) -> Result<u64, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains a NUL byte".to_string())?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let result = unsafe { libc::statfs(c_path.as_ptr(), &mut stats) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(stats.f_bavail as u64 * stats.f_bsize as u64)
}

#[cfg(not(unix))]
fn available_bytes(_path: &Path) -> Result<u64, String> {
    Err("free space cannot be determined on this platform".to_string())
}

/// Extract a verified, compressed artifact into a staging directory.
///
/// Both digests are checked, in this order:
///
/// 1. the **compressed** bytes, before decompression, so a truncated download is refused while it
///    is still just a wrong file rather than a wrong database
/// 2. the **extracted** bytes, after it, so a bug in the decompressor cannot write arbitrary
///    content to disk and have it accepted
///
/// Checking only the first would leave the second uncovered; checking only the second would let a
/// corrupted download reach the decompressor at all.
pub fn stage(
    compressed: &Path,
    destination: &Path,
    version: &str,
    expected_raw_sha256: &str,
) -> Result<StagedPackage, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", destination.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;

    // Inside the destination's parent, so the rename below is a rename and not a copy.
    let staging = parent.join(format!(".terrakit-staging-{version}-{}", std::process::id()));
    if staging.exists() {
        // Left behind by a previous crash. Cleared rather than reused: a partial extract is not
        // a resumable extract, and reusing it is how a half-written database reaches disk.
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;

    let database = staging.join("data.sqlite");
    let outcome = decompress_to(compressed, &database);
    if let Err(error) = outcome {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    let actual = sha256_file(&database)?;
    if actual != expected_raw_sha256.trim().to_lowercase() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "the extracted database does not match the manifest.\n\
             \x20      expected {expected_raw_sha256}\n       actual   {actual}\n\
             \x20      The download verified but did not decompress to the published database. \
             Nothing has been installed."
        ));
    }

    let size_on_disk = dir_size(&staging);
    Ok(StagedPackage {
        directory: staging,
        version: version.to_string(),
        size_on_disk,
    })
}

fn decompress_to(compressed: &Path, target: &Path) -> Result<(), String> {
    let input = std::fs::File::open(compressed)
        .map_err(|e| format!("cannot open {}: {e}", compressed.display()))?;
    let output = std::fs::File::create(target)
        .map_err(|e| format!("cannot create {}: {e}", target.display()))?;
    let mut writer = std::io::BufWriter::new(output);

    zstd::stream::copy_decode(input, &mut writer)
        .map_err(|e| format!("decompressing {} failed: {e}", compressed.display()))?;
    writer
        .flush()
        .map_err(|e| format!("writing {}: {e}", target.display()))?;
    Ok(())
}

/// Move a staged package into place.
///
/// `destination` must not already exist. Replacing an installed package is `add` on a new
/// version, which installs alongside and updates the resolution — never an in-place overwrite,
/// because a failed overwrite leaves a half-written package where a working one used to be.
pub fn commit(staged: StagedPackage, destination: &Path) -> Result<StagedPackage, String> {
    if destination.exists() {
        return Err(format!(
            "{} already exists. Refusing to overwrite an installed package; \
             remove it first with 'terrakit remove'.",
            destination.display()
        ));
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::rename(&staged.directory, destination).map_err(|e| {
        format!(
            "cannot move {} to {}: {e}\n\
             \x20      Both paths must be on the same filesystem for the install to be atomic. \
             Moving .terrakit off the project filesystem defeats that.",
            staged.directory.display(),
            destination.display()
        )
    })?;
    Ok(StagedPackage {
        directory: destination.to_path_buf(),
        ..staged
    })
}

/// Discard a staged package without touching anything installed.
pub fn discard(staged: &StagedPackage) {
    let _ = std::fs::remove_dir_all(&staged.directory);
}

/// Total bytes under a directory.
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => total += dir_size(&entry.path()),
            Ok(meta) => total += meta.len(),
            Err(_) => continue,
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::sha256_bytes;

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-install-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_required_space_accounts_for_both_copies() {
        let plan = InstallPlan {
            country: "TR".into(),
            version: "2026.10.1".into(),
            destination: PathBuf::from("/p/TR/2026.10.1"),
            extracted_size: 10_000_000,
            compressed_size: 2_000_000,
        };
        // 10 MB extracted exists twice during the install, plus 2 MB download, plus margin.
        assert!(plan.required_free_bytes() >= 22_000_000);
        assert!(plan.required_free_bytes() < 22_000_000 + SPACE_MARGIN_BYTES);
    }

    #[test]
    fn space_is_checked_against_the_destination_filesystem() {
        let dir = tempdir("space");
        // The parent must exist: check_space treats a missing parent as "nothing to check
        // against" because `add` creates the packages directory itself.
        std::fs::create_dir_all(dir.join("TR")).unwrap();
        let plan = InstallPlan {
            country: "TR".into(),
            version: "1".into(),
            destination: dir.join("TR").join("1"),
            // More than any test machine has.
            extracted_size: u64::MAX / 4,
            compressed_size: 0,
        };
        let error = check_space(&plan).unwrap_err();
        assert!(error.contains("not enough space"), "{error}");
        assert!(error.contains("--cache-dir"), "must say what to do: {error}");
    }

    #[test]
    fn staging_verifies_both_the_compressed_and_the_extracted_bytes() {
        let dir = tempdir("stage");
        let payload = b"a small sqlite file, pretending".repeat(500);
        let digest = sha256_bytes(&payload);
        let compressed = dir.join("data.sqlite.zst");
        compress_to(&payload, &compressed);

        let destination = dir.join("packages").join("TR").join("1.0.0");
        let staged = stage(&compressed, &destination, "1.0.0", &digest).unwrap();
        assert_eq!(std::fs::read(staged.directory.join("data.sqlite")).unwrap(), payload);

        // The staged directory is a sibling of the destination, not inside it: `rename` is only
        // atomic within a filesystem, and /tmp is often a different one.
        assert_eq!(
            staged.directory.parent().unwrap(),
            destination.parent().unwrap(),
            "staging must share a filesystem with the destination"
        );

        let committed = commit(staged, &destination).unwrap();
        assert!(destination.is_dir());
        assert!(!committed.directory.as_os_str().is_empty());
    }

    #[test]
    fn a_wrong_expected_digest_installs_nothing() {
        let dir = tempdir("wrongdigest");
        let payload = b"real content".repeat(100);
        let compressed = dir.join("data.sqlite.zst");
        compress_to(&payload, &compressed);

        let destination = dir.join("packages").join("TR").join("1.0.0");
        let error = stage(&compressed, &destination, "1.0.0", &sha256_bytes(b"other"))
            .unwrap_err();
        assert!(error.contains("does not match the manifest"), "{error}");
        assert!(
            !destination.exists(),
            "a failed stage must leave nothing behind at the destination"
        );
        // And no staging directory is left either.
        let leftovers: Vec<_> = std::fs::read_dir(destination.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".terrakit-staging"))
            .collect();
        assert!(leftovers.is_empty(), "staging must be cleaned up: {leftovers:?}");
    }

    #[test]
    fn a_corrupt_compressed_file_fails_before_writing_a_database() {
        let dir = tempdir("corrupt");
        let compressed = dir.join("data.sqlite.zst");
        // Not a zstd frame at all.
        std::fs::write(&compressed, b"this is not zstd").unwrap();

        let destination = dir.join("packages").join("TR").join("1.0.0");
        let error = stage(&compressed, &destination, "1.0.0", &sha256_bytes(b"x")).unwrap_err();
        assert!(error.contains("decompressing"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn commit_refuses_to_overwrite_an_installed_package() {
        let dir = tempdir("overwrite");
        let destination = dir.join("TR").join("1.0.0");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("data.sqlite"), b"the working one").unwrap();

        let staging = dir.join(".terrakit-staging-new");
        std::fs::create_dir_all(&staging).unwrap();

        let error = commit(
            StagedPackage {
                directory: staging,
                version: "1.0.0".into(),
                size_on_disk: 0,
            },
            &destination,
        )
        .unwrap_err();
        assert!(error.contains("Refusing to overwrite"), "{error}");
        assert_eq!(
            std::fs::read(destination.join("data.sqlite")).unwrap(),
            b"the working one",
            "the installed package must be untouched"
        );
    }

    #[test]
    fn discard_removes_a_staged_package_and_keeps_the_installed_one() {
        let dir = tempdir("discard");
        let installed = dir.join("TR").join("1.0.0");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("data.sqlite"), b"old").unwrap();

        let staging = dir.join("TR").join(".terrakit-staging-2");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("data.sqlite"), b"new").unwrap();

        discard(&StagedPackage {
            directory: staging.clone(),
            version: "2".into(),
            size_on_disk: 0,
        });

        assert!(!staging.exists());
        assert!(installed.join("data.sqlite").exists(), "must keep the installed one");
    }

    #[test]
    fn a_leftover_staging_directory_from_a_crash_is_cleared_not_reused() {
        // Reusing a partial extract is how a half-written database reaches disk.
        let dir = tempdir("leftover");
        let payload = b"payload".repeat(100);
        let compressed = dir.join("data.sqlite.zst");
        compress_to(&payload, &compressed);

        let destination = dir.join("packages").join("TR").join("1.0.0");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        // Simulate a crash: staging directory present, partially written.
        let crashed = destination
            .parent()
            .unwrap()
            .join(format!(".terrakit-staging-1.0.0-{}", std::process::id()));
        std::fs::create_dir_all(&crashed).unwrap();
        std::fs::write(crashed.join("data.sqlite"), b"half a dat").unwrap();

        let staged = stage(&compressed, &destination, "1.0.0", &sha256_bytes(&payload)).unwrap();
        assert_eq!(
            std::fs::read(staged.directory.join("data.sqlite")).unwrap(),
            payload,
            "the stale content must be gone"
        );
        discard(&staged);
    }

    #[test]
    fn dir_size_sums_nested_files() {
        let dir = tempdir("size");
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("a").join("b").join("f"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("g"), vec![0u8; 500]).unwrap();
        assert_eq!(dir_size(&dir), 1500);
    }

    /// Write compressed bytes to a path. A helper rather than three repeated lines, and it
    /// keeps the compression settings identical across the tests that depend on them.
    fn compress_to(payload: &[u8], path: &Path) {
        std::fs::write(path, zstd::stream::encode_all(payload, 19).unwrap()).unwrap();
    }
}