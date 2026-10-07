//! `terrakit cache` — inspect, verify and prune the shared download cache.
//!
//! The cache is shared across every project on the machine, which makes two of its operations
//! genuinely dangerous and worth getting right rather than merely implementing.
//!
//! **It cannot know what other projects reference.** Only this project's installed packages are
//! visible, so "unreferenced" here means "not referenced by anything this project can see". An
//! entry another project installed five minutes ago is unreferenced by definition. The mitigation
//! is a recency floor: an entry younger than [`PRUNE_MIN_AGE`] is left alone regardless, because
//! the overwhelmingly common reason an entry is young is that something is installing or has just
//! installed it. `clean --all` is the honest full delete — safe, since a cache can always be
//! refilled, only slower.
//!
//! **A cache entry is a claim, not a fact.** Filenames are digests, but an interrupted write, a
//! truncated file or a poisoned entry would otherwise be served silently, because the fast path
//! skips the download and with it the checksum. `add` re-verifies before use and `cache verify`
//! exists to re-check everything at once.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::exit::Exit;
use crate::package::Project;
use crate::verify::{hex_to_bytes, sha256_file};

/// An unreferenced entry younger than this is left alone.
///
/// Not a heuristic about disk pressure — disk is cheap — but about *concurrency*: an entry this
/// fresh almost certainly belongs to an install in progress or to another project that installed
/// it moments ago and which this command cannot see.
pub const PRUNE_MIN_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One cached artifact.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub digest: String,
    pub path: PathBuf,
    pub size: u64,
    pub age: Duration,
    /// True when an installed package in this project names this digest.
    pub referenced: bool,
    /// The digests this entry's own name claims, split out for reporting.
    pub looks_like_digest: bool,
}

impl CacheEntry {
    /// Whether `clean` may remove it.
    pub fn prunable(&self) -> bool {
        !self.referenced && self.age >= PRUNE_MIN_AGE
    }

    /// Why it was kept, for the `--verbose` line.
    pub fn keep_reason(&self) -> Option<&'static str> {
        if self.referenced {
            return Some("referenced by an installed package");
        }
        if self.age < PRUNE_MIN_AGE {
            return Some("too recent to be unreferenced");
        }
        None
    }
}

fn now() -> SystemTime {
    SystemTime::now()
}

/// Every artifact in the cache, with its metadata.
///
/// Digests are parsed from filenames so a corrupt entry is *visible* rather than skipped. An
/// implementation that only listed well-formed names would report a clean cache while a
/// truncated file sat in it.
pub fn list(cache_dir: &Path, project: Option<&Project>) -> Vec<CacheEntry> {
    let referenced = project.map(referenced_digests).unwrap_or_default();
    let mut out = Vec::new();

    let artifacts = cache_dir.join("artifacts");
    let Ok(shards) = std::fs::read_dir(&artifacts) else {
        return out;
    };

    let current = now();
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(digest) = name.split('.').next() else {
                continue;
            };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            out.push(CacheEntry {
                looks_like_digest: digest.len() == 64 && hex_to_bytes(digest).is_ok(),
                referenced: referenced.contains(digest),
                digest: digest.to_string(),
                path: entry.path(),
                size,
                age: current.duration_since(modified).unwrap_or(Duration::ZERO),
            });
        }
    }

    // Newest first, then by digest, so the output is stable between runs on an unchanged cache.
    out.sort_by(|a, b| {
        b.age
            .cmp(&a.age)
            .then_with(|| a.digest.cmp(&b.digest))
    });
    out
}

/// Digests named by any installed package in this project.
pub fn referenced_digests(project: &Project) -> std::collections::HashSet<String> {
    let mut referenced = std::collections::HashSet::new();
    for package in project.installed() {
        let Ok(raw) = std::fs::read(&package.manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(files) = manifest.get("files").and_then(|f| f.as_array()) else {
            continue;
        };
        for file in files {
            // Both digests: the compressed artifact is what is cached, and the extracted one is
            // recorded alongside it. Naming only the compressed digest would leave an entry
            // prunable in a project that stored the other form.
            for field in ["sha256", "compressedSha256"] {
                if let Some(digest) = file.get(field).and_then(|v| v.as_str()) {
                    referenced.insert(digest.to_string());
                }
            }
        }
    }
    referenced
}

/// Result of a prune.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub removed: Vec<String>,
    pub kept: Vec<(String, &'static str)>,
    pub freed: u64,
}

impl PruneReport {
    pub fn removed_count(&self) -> usize {
        self.removed.len()
    }
}

/// Remove prunable entries.
///
/// `all` deletes every artifact regardless of age or reference. That is safe — a cache is
/// reconstructible — and it is the only operation here whose cost is paid by something other than
/// the person who ran it.
pub fn clean(cache_dir: &Path, project: Option<&Project>, all: bool) -> Result<PruneReport, String> {
    let mut report = PruneReport::default();

    for entry in list(cache_dir, project) {
        if !all && !entry.prunable() {
            if let Some(reason) = entry.keep_reason() {
                report.kept.push((entry.digest.clone(), reason));
            }
            continue;
        }
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                report.freed += entry.size;
                report.removed.push(entry.digest.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!("cannot remove {}: {e}", entry.path.display()));
            }
        }
    }

    // Empty shard directories would otherwise accumulate one per digest prefix.
    if let Ok(shards) = std::fs::read_dir(cache_dir.join("artifacts")) {
        for shard in shards.flatten() {
            if shard.path().is_dir() {
                let _ = std::fs::remove_dir(shard.path());
            }
        }
    }

    Ok(report)
}

/// One verification failure.
#[derive(Debug, Clone)]
pub struct VerifyFailure {
    pub digest: String,
    pub expected: String,
    pub actual: String,
    pub reason: String,
}

/// Re-hash every cached artifact.
///
/// The name of a cache entry is its digest, so a mismatch means the file was truncated, partially
/// overwritten, or written by something else. Every such entry is deleted: a cache entry that
/// fails its own checksum has no value, and leaving it means the next `add` re-verifies and fails
/// again, having wasted a download to get there.
pub fn verify(cache_dir: &Path) -> Result<(usize, u64, Vec<VerifyFailure>), String> {
    let entries = list(cache_dir, None);
    let mut checked = 0usize;
    let mut bytes = 0u64;
    let mut failures = Vec::new();

    for entry in &entries {
        checked += 1;
        bytes += entry.size;

        if !entry.looks_like_digest {
            failures.push(VerifyFailure {
                digest: entry.digest.clone(),
                expected: "64 hex characters".to_string(),
                actual: entry.digest.clone(),
                reason: "the filename is not a sha256 digest".to_string(),
            });
            let _ = std::fs::remove_file(&entry.path);
            continue;
        }

        match sha256_file(&entry.path) {
            Ok(actual) if actual == entry.digest => {}
            Ok(actual) => {
                failures.push(VerifyFailure {
                    digest: entry.digest.clone(),
                    expected: entry.digest.clone(),
                    actual,
                    reason: "the cached bytes do not match the digest in the filename".to_string(),
                });
                let _ = std::fs::remove_file(&entry.path);
            }
            Err(e) => {
                failures.push(VerifyFailure {
                    digest: entry.digest.clone(),
                    expected: entry.digest.clone(),
                    actual: "(unreadable)".to_string(),
                    reason: e,
                });
                let _ = std::fs::remove_file(&entry.path);
            }
        }
    }

    Ok((checked, bytes, failures))
}

/// Format a duration compactly.
///
/// Precise enough to be useful, short enough for a table.
pub fn human_age(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h", seconds / 3600);
    }
    let days = seconds / 86_400;
    if days < 365 {
        return format!("{days}d");
    }
    format!("{}y", days / 365)
}

/// Map a cache command's outcome onto an exit code.
///
/// A failed verification is exit 3, matching every other verification failure in the CLI, so a CI
/// script needs no special case for a corrupt cache.
pub fn exit_for_verify(failure_count: usize) -> Exit {
    if failure_count > 0 {
        Exit::Verification
    } else {
        Exit::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-cache-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn seed(cache: &Path, digest: &str, bytes: &[u8]) -> PathBuf {
        let shard = cache.join("artifacts").join(&digest[..2]);
        std::fs::create_dir_all(&shard).unwrap();
        let path = shard.join(format!("{digest}.zst"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Backdate a file so it clears `PRUNE_MIN_AGE`.
    ///
    /// Uses `File::set_times`, stable since Rust 1.75, rather than shelling out to `touch -t`,
    /// which wants a formatted timestamp rather than a unix one and would silently do nothing
    /// useful if it got one.
    fn old_enough(path: &Path) {
        let target = SystemTime::now() - (PRUNE_MIN_AGE + Duration::from_secs(3600));
        let file = std::fs::File::options().write(true).open(path).expect("seeded file");
        file.set_times(std::fs::FileTimes::new().set_modified(target))
            .expect("setting mtime");
    }

    #[test]
    fn a_missing_cache_directory_is_empty_not_an_error() {
        let absent = PathBuf::from("/tmp/terrakit-cache-does-not-exist-xyz");
        assert!(list(&absent, None).is_empty());
        assert!(clean(&absent, None, false).is_ok());
        assert!(verify(&absent).unwrap().0 == 0);
    }

    #[test]
    fn entries_are_listed_with_size_and_recency() {
        let cache = tempdir("list");
        let digest = crate::verify::sha256_bytes(b"payload");
        let path = seed(&cache, &digest, b"payload");

        let entries = list(&cache, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].digest, digest);
        assert_eq!(entries[0].size, 7);
        assert!(entries[0].looks_like_digest);
        assert!(!entries[0].referenced);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn clean_keeps_referenced_entries() {
        let cache = tempdir("referenced");
        let digest = crate::verify::sha256_bytes(b"payload");
        let path = seed(&cache, &digest, b"payload");
        old_enough(&path);

        // No project, so nothing is referenced: it is prunable.
        assert!(clean(&cache, None, false).unwrap().removed_count() == 1);
        assert!(!path.exists());

        // Seed again and pretend a project references it.
        seed(&cache, &digest, b"payload");
        let project = Project::at(tempdir("referenced-project"));
        std::fs::create_dir_all(project.packages_dir.join("TR").join("1.0.0")).unwrap();
        std::fs::write(
            project.packages_dir.join("TR").join("1.0.0").join("data.sqlite"),
            b"db",
        )
        .unwrap();
        std::fs::write(
            project.packages_dir.join("TR").join("1.0.0").join("manifest.json"),
            format!(r#"{{"files":[{{"path":"data.sqlite","sha256":"{digest}"}}]}}"#),
        )
        .unwrap();

        let report = clean(&cache, Some(&project), false).unwrap();
        assert_eq!(report.removed_count(), 0, "a referenced digest must survive");
        assert!(
            report.kept.iter().any(|(_, reason)| reason.contains("referenced")),
            "and the reason must be reported: {:?}",
            report.kept
        );
    }

    #[test]
    fn clean_keeps_recent_entries_because_another_project_may_own_them() {
        // The cache cannot see other projects, so a fresh entry is unreferenced by definition.
        // Deleting it would turn another project's next install into a download for no reason.
        let cache = tempdir("recent");
        let digest = crate::verify::sha256_bytes(b"payload");
        let path = seed(&cache, &digest, b"payload");

        let report = clean(&cache, None, false).unwrap();
        assert_eq!(report.removed_count(), 0, "a just-created entry must survive");
        assert!(path.exists());
        assert!(report.kept.iter().any(|(_, r)| r.contains("too recent")));
    }

    #[test]
    fn clean_all_removes_everything_regardless() {
        let cache = tempdir("all");
        let one = crate::verify::sha256_bytes(b"one");
        let two = crate::verify::sha256_bytes(b"two");
        seed(&cache, &one, b"one");
        seed(&cache, &two, b"two");

        let report = clean(&cache, None, true).unwrap();
        assert_eq!(report.removed_count(), 2);
        assert_eq!(report.freed, 6);
        assert!(list(&cache, None).is_empty());
    }

    #[test]
    fn clean_removes_the_shard_directories_it_empties() {
        // 256 shards per prefix would otherwise accumulate as a slow leak.
        let cache = tempdir("shards");
        let digest = crate::verify::sha256_bytes(b"payload");
        seed(&cache, &digest, b"payload");
        clean(&cache, None, true).unwrap();
        assert!(!cache.join("artifacts").join(&digest[..2]).exists());
    }

    #[test]
    fn verify_passes_a_good_cache_and_reports_the_count() {
        let cache = tempdir("verify-good");
        let payload = b"a real artifact".repeat(100);
        let digest = crate::verify::sha256_bytes(&payload);
        seed(&cache, &digest, &payload);

        let (checked, bytes, failures) = verify(&cache).unwrap();
        assert_eq!(checked, 1);
        assert_eq!(bytes, payload.len() as u64);
        assert!(failures.is_empty());
        assert_eq!(exit_for_verify(failures.len()), Exit::Ok);
    }

    #[test]
    fn verify_detects_and_removes_a_corrupted_entry() {
        let cache = tempdir("verify-bad");
        let digest = crate::verify::sha256_bytes(b"the original bytes");
        let path = seed(&cache, &digest, b"the original bytes");

        // Truncated by an interrupted write, which is the realistic cause.
        std::fs::write(&path, b"the orig").unwrap();

        let (_, _, failures) = verify(&cache).unwrap();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].actual != digest);
        assert!(failures[0].reason.contains("do not match"), "{:?}", failures[0]);
        assert!(
            !path.exists(),
            "an entry that fails its own digest has no value and must be dropped, \
             or the next add re-downloads only to fail again"
        );
        assert_eq!(exit_for_verify(failures.len()), Exit::Verification);
    }

    #[test]
    fn verify_reports_a_filename_that_is_not_a_digest() {
        // Silently skipping it would report a clean cache while a junk file sat in it.
        let cache = tempdir("verify-junk");
        let shard = cache.join("artifacts").join("zz");
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("not-a-digest.zst"), b"junk").unwrap();

        let (checked, _, failures) = verify(&cache).unwrap();
        assert_eq!(checked, 1);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("not a sha256 digest"));
    }

    #[test]
    fn verify_checks_both_digest_fields_of_an_installed_manifest() {
        // A manifest records the extracted digest and the compressed one. Naming only the
        // compressed digest would let an entry referenced by the other form be pruned.
        let _cache = tempdir("both-digests");
        let project = Project::at(tempdir("both-digests-project"));
        let dir = project.packages_dir.join("TR").join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.sqlite"), b"db").unwrap();

        let compressed = "b".repeat(64);
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"files":[{{"path":"data.sqlite","sha256":"{}","compressedSha256":"{compressed}"}}]}}"#,
                "a".repeat(64)
            ),
        )
        .unwrap();

        let referenced = referenced_digests(&project);
        assert!(referenced.contains(&compressed));
        assert!(referenced.contains(&"a".repeat(64)));
    }

    #[test]
    fn ages_are_rendered_at_a_useful_precision() {
        assert_eq!(human_age(Duration::from_secs(30)), "30s");
        assert_eq!(human_age(Duration::from_secs(600)), "10m");
        assert_eq!(human_age(Duration::from_secs(7_200)), "2h");
        assert_eq!(human_age(Duration::from_secs(172_800)), "2d");
        assert_eq!(human_age(Duration::from_secs(63_072_000)), "2y");
    }
}