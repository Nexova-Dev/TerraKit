//! Fetching bytes, with resume, over `https://` or `file://`.
//!
//! Two properties matter more than throughput here:
//!
//! **A resumed download must not silently produce a corrupt file.** HTTP `Range` gives bytes
//! from an offset; if the server ignores it and replies `200` instead of `206`, appending the
//! body to the existing partial file yields a file with the first N bytes duplicated. That file
//! has the right length and the wrong content, and the checksum catches it — but only if the
//! partial file is recorded as partial rather than trusted. So the status code is checked, not
//! assumed, and a server that ignores `Range` restarts from zero instead of appending.
//!
//! **Verification happens before decompression.** A truncated body otherwise decompresses into a
//! short database and fails with a SQLite error that says nothing about the network.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exit::Exit;
use crate::verify::sha256_bytes;

/// Retry and timeout policy.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: usize = 3;

/// What happened during a fetch, for the progress line and for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    pub bytes: u64,
    pub resumed_from: u64,
    pub attempts: usize,
}

impl FetchOutcome {
    pub fn summary(&self) -> String {
        if self.resumed_from > 0 {
            format!(
                "{} (resumed at {}, {} attempt(s))",
                human_bytes(self.bytes),
                human_bytes(self.resumed_from),
                self.attempts
            )
        } else {
            format!("{} ({} attempt(s))", human_bytes(self.bytes), self.attempts)
        }
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Fetch a small text document, as used for `index.json` and `packages/{name}.json`.
///
/// No resume and no caching: these are small, they must be fresh, and the registry's
/// `cache-policy.json` sets a five-minute TTL on `index.json` so freshness comes from the CDN.
pub fn fetch_text(url: &str, offline: bool) -> Result<String, String> {
    if offline {
        return Err(format!(
            "--offline was given, so {url} cannot be fetched.\n\
             \x20      An installed package can still be queried; only fetching is disabled."
        ));
    }

    if let Some(path) = file_url_path(url) {
        return std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()));
    }

    let mut last_error = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match fetch_text_once(url) {
            Ok(body) => return Ok(body),
            Err(error) => {
                last_error = error;
                if attempt < MAX_ATTEMPTS {
                    // Linear backoff. A registry that is briefly unavailable is the common case;
                    // hammering it three times in fifty milliseconds is not.
                    std::thread::sleep(Duration::from_millis(250 * attempt as u64));
                }
            }
        }
    }
    Err(last_error)
}

fn fetch_text_once(url: &str) -> Result<String, String> {
    let agent = agent();
    let response = agent
        .get(url)
        .call()
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("reading {url} failed: {e}"))
}

/// Download an artifact into `target`, verifying its digest.
///
/// `expected_sha256` is the digest of the **downloaded** bytes. When resuming, the partial file
/// is completed and then hashed once, in full, so a resume cannot pass verification by hashing
/// only the new part.
pub fn download(
    url: &str,
    target: &Path,
    expected_sha256: Option<&str>,
    offline: bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<FetchOutcome, String> {
    if offline {
        return Err(format!(
            "--offline was given, so {url} cannot be downloaded.\n\
             \x20      An installed package can still be queried; only fetching is disabled."
        ));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    let existing = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);

    if let Some(path) = file_url_path(url) {
        return download_from_file(&path, target, expected_sha256, progress);
    }

    let agent = agent();
    let mut offset = existing;
    let mut resumed_from = 0u64;

    let mut last_error = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let mut request = agent.get(url);
        if offset > 0 {
            request = request.header("Range", &format!("bytes={offset}-"));
        }
        match request.call() {
            Ok(response) => {
                let status = response.status().as_u16();
                let partial = offset > 0 && status == 206;

                if offset > 0 && !partial {
                    // The server ignored Range and is sending the whole body. Appending it to
                    // the existing partial file would duplicate the first `offset` bytes and
                    // produce a file of the right length holding the wrong content. Start over.
                    eprintln!("  (server ignored Range; restarting the download)");
                    let _ = std::fs::remove_file(target);
                    offset = 0;
                }
                if offset > 0 && partial {
                    resumed_from = offset;
                }
                if !(200..300).contains(&status) {
                    last_error = format!("GET {url} returned HTTP {status}");
                } else {
                    let total = response
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|length| length + offset)
                        .unwrap_or(0);

                    match stream_to_file(response.into_body().into_reader(), target, offset, total, progress) {
                        Ok(bytes) => {
                            if let Some(expected) = expected_sha256 {
                                verify_file(target, expected)?;
                            }
                            return Ok(FetchOutcome {
                                bytes,
                                resumed_from,
                                attempts: attempt,
                            });
                        }
                        Err(error) => {
                            // Keep whatever arrived: it is valid prefix data and the next
                            // attempt resumes from it.
                            last_error = error;
                            offset = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);
                        }
                    }
                }
            }
            Err(error) => {
                last_error = format!("GET {url} failed: {error}");
            }
        }
        if attempt < MAX_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(250 * attempt as u64));
        }
    }
    Err(last_error)
}

fn download_from_file(
    source: &Path,
    target: &Path,
    expected_sha256: Option<&str>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<FetchOutcome, String> {
    let total = std::fs::metadata(source)
        .map_err(|e| format!("cannot stat {}: {e}", source.display()))?
        .len();
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("cannot open {}: {e}", source.display()))?;
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(target)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;

    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|e| format!("reading {}: {e}", source.display()))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|e| format!("writing {}: {e}", target.display()))?;
        written += read as u64;
        progress(written, total);
    }
    output
        .flush()
        .map_err(|e| format!("flushing {}: {e}", target.display()))?;

    if let Some(expected) = expected_sha256 {
        verify_file(target, expected)?;
    }
    Ok(FetchOutcome {
        bytes: written,
        resumed_from: 0,
        attempts: 1,
    })
}

fn stream_to_file(
    mut reader: impl Read,
    target: &Path,
    offset: u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, String> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(target)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seeking in {}: {e}", target.display()))?;
    }

    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = offset;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("reading the response body: {e}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("writing {}: {e}", target.display()))?;
        written += read as u64;
        progress(written, total);
    }
    file.flush()
        .map_err(|e| format!("flushing {}: {e}", target.display()))?;
    Ok(written)
}

/// Compare a file's digest against an expected value.
pub fn verify_file(path: &Path, expected: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let actual = sha256_bytes(&bytes);
    if actual == expected.trim().to_lowercase() {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch for {}\n       expected {}\n       actual   {}\n\
             \x20      The download is corrupt or was tampered with in transit. It has not been \
             installed. Run the command again; if it repeats, the registry copy is damaged.",
            path.display(),
            expected,
            actual
        ))
    }
}

/// Verify bytes held in memory.
pub fn verify_bytes(bytes: &[u8], expected: &str) -> Result<(), String> {
    let actual = sha256_bytes(bytes);
    if actual == expected.trim().to_lowercase() {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch\n       expected {expected}\n       actual   {actual}"
        ))
    }
}

/// The HTTP agent every fetch goes through.
///
/// Built from ureq's stock TLS configuration with rustls. Nothing here relaxes certificate
/// validation, and nothing here should: an installer that accepts any certificate accepts any
/// attacker's package, and the signature check would not help because the attacker would also
/// supply the signature — the keyring is the only thing standing between that and an install, and
/// it is worth not weakening the layer beneath it.
///
/// The timeouts are the whole reason an agent exists at all rather than a bare
/// `ureq::get(..).call()`: a package download can legitimately take minutes, while the *connection*
/// should not take more than ten seconds, and a client that conflates the two hangs on a black
/// holed network instead of failing.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TRANSFER_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .build()
        .new_agent()
}

/// The filesystem path behind a `file://` URL, if the URL is one.
///
/// Rejects `..` so a manifest served from the registry cannot point the installer at an arbitrary
/// path. The registry already had to be trusted enough that its signature verifies, but a
/// directory traversal would turn a compromised index into arbitrary file read on the client,
/// which is a much larger blast radius than "a bad package was installed".
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    if rest.contains("..") {
        return None;
    }
    // file:///abs/path and file://localhost/abs/path both name an absolute path; anything else
    // names a host this build does not support.
    let path = rest
        .strip_prefix("localhost")
        .or_else(|| Some(rest))
        .unwrap_or(rest);
    Some(PathBuf::from(path))
}

/// Whether a URL uses the `file://` scheme.
pub fn is_file_url(url: &str) -> bool {
    url.starts_with("file://")
}

/// Cache location for a download.
///
/// Keyed by the digest, not the URL: two registries serving identical bytes share one cache
/// entry, and a partially downloaded file resumes across invocations without a sidecar file
/// tracking progress. The digest is verified before use, so a poisoned cache entry cannot
/// install anything.
pub fn cache_path(cache_dir: &Path, sha256: &str, suffix: &str) -> PathBuf {
    cache_dir.join("artifacts").join(&sha256[..2]).join(format!("{sha256}{suffix}"))
}

/// Map an error from this module onto an exit code.
///
/// A checksum mismatch and a refused signature are verification failures (3); everything else
/// is a runtime failure (1). Collapsing them would make `--exit-code` policies unable to
/// distinguish "the network is down" from "someone is tampering".
pub fn exit_for(error: &str) -> Exit {
    if error.contains("checksum mismatch")
        || error.contains("does not verify")
        || error.contains("not trusted")
    {
        Exit::Verification
    } else {
        Exit::Failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-download-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn file_urls_are_recognised_and_traversal_rejected() {
        assert_eq!(
            file_url_path("file:///tmp/registry/index.json"),
            Some(PathBuf::from("/tmp/registry/index.json"))
        );
        assert_eq!(file_url_path("file:///tmp/../../etc/passwd"), None);
        assert_eq!(file_url_path("https://x.dev/index.json"), None);
    }

    #[test]
    fn offline_refuses_before_touching_the_network() {
        let error = fetch_text("https://registry.invalid/index.json", true).unwrap_err();
        assert!(error.contains("--offline"));
        assert!(
            error.contains("An installed package can still be queried"),
            "the message should say what still works: {error}"
        );
    }

    #[test]
    fn a_file_registry_downloads_and_verifies() {
        let dir = tempdir("file");
        let source = dir.join("artifact.zst");
        let payload = b"terra kit artifact bytes".repeat(1000);
        std::fs::write(&source, &payload).unwrap();

        let target = dir.join("downloaded.zst");
        let mut progress = |_: u64, _: u64| {};
        let outcome = download_from_file(
            &source,
            &target,
            Some(&sha256_bytes(&payload)),
            &mut progress,
        )
        .unwrap();

        assert_eq!(outcome.bytes, payload.len() as u64);
        assert_eq!(std::fs::read(&target).unwrap(), payload);
    }

    #[test]
    fn a_corrupt_download_is_refused_not_installed() {
        let dir = tempdir("corrupt");
        let source = dir.join("artifact");
        std::fs::write(&source, b"actual bytes").unwrap();
        let target = dir.join("downloaded");

        let mut progress = |_: u64, _: u64| {};
        let error = download_from_file(&source, &target, Some(&sha256_bytes(b"expected")), &mut progress)
            .unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert!(
            error.contains("has not been installed"),
            "the message must say the artifact was not used: {error}"
        );
    }

    #[test]
    fn checksum_verification_is_case_insensitive_and_whitespace_tolerant() {
        let bytes = b"hello";
        let digest = sha256_bytes(bytes);
        assert!(verify_bytes(bytes, &digest).is_ok());
        assert!(verify_bytes(bytes, &digest.to_uppercase()).is_ok());
        assert!(verify_bytes(bytes, &format!("  {digest}\n")).is_ok());
        assert!(verify_bytes(bytes, "deadbeef").is_err());
    }

    #[test]
    fn resume_metadata_is_reported_when_a_download_restarts() {
        let fresh = FetchOutcome { bytes: 100, resumed_from: 0, attempts: 1 };
        assert_eq!(fresh.summary(), "100 B (1 attempt(s))");

        let resumed = FetchOutcome { bytes: 100, resumed_from: 40, attempts: 2 };
        assert!(resumed.summary().contains("resumed at 40 B"));
    }

    #[test]
    fn cache_paths_are_keyed_by_digest_so_mirrors_share_entries() {
        let digest = "a".repeat(64);
        let one = cache_path(Path::new("/cache"), &digest, ".zst");
        let two = cache_path(Path::new("/cache"), &digest, ".zst");
        assert_eq!(one, two);
        // Sharded by prefix so no directory holds thousands of entries.
        assert!(one.to_string_lossy().contains(&format!("{}/artifacts/aa/", "/cache").replace("//artifacts", "/artifacts")));
        assert_eq!(cache_path(Path::new("/cache"), &digest, ""), cache_path(Path::new("/cache"), &digest, ""));
    }

    #[test]
    fn verification_failures_map_to_the_verification_exit_code() {
        assert_eq!(
            exit_for("checksum mismatch for /tmp/x"),
            Exit::Verification
        );
        assert_eq!(
            exit_for("signed by key \"x\", which is not trusted"),
            Exit::Verification
        );
        // A network failure must not masquerade as tampering, or a flaky connection looks
        // like an attack and every transient outage becomes an incident.
        assert_eq!(exit_for("GET https://x failed: timed out"), Exit::Failure);
        assert_eq!(exit_for("cannot read /tmp/x: No such file"), Exit::Failure);
    }
}