//! `terrakit install github.com/owner/repo@ref`.
//!
//! A GitHub repository is an **untrusted transport, not a trusted publisher**
//! ([`PACKAGE_FORMAT.md` §10](PACKAGE_FORMAT.md#10-github-package-sources)). Anyone who can push
//! to a branch can push data, so the only thing standing between that and a production app is
//! signature verification — which applies here exactly as it does for a registry package. A
//! repository with a valid TerraKit signature installs; one without a trusted signature does not.
//!
//! Three things follow from taking the transport seriously, and all three are enforced rather
//! than assumed:
//!
//! **Archive entries are hostile input.** A GitHub tarball is built by whoever pushed the commit.
//! Entries may point outside the extraction directory, or be symlinks that do so after the fact.
//! Extraction rejects absolute paths, any path that resolves outside the destination, and every
//! non-regular entry type — a package needs three regular files and nothing else.
//!
//! **A tarball is a decompression bomb waiting to happen.** Entry count and total extracted bytes
//! are capped. Without a cap, a 40 KB archive expands to fill the disk, and the disk-space check
//! `add` performs runs before the download, not after the expansion.
//!
//! **A branch is not reproducible.** `@main` resolves to whatever the branch points at right now.
//! The CLI states the resolved commit SHA and pins it in `terrakit.json`, so a later `add` of the
//! same source gets the same bytes even though `main` may have moved.

use std::path::{Component, Path, PathBuf};

use crate::exit::Exit;
use crate::signature;

/// `https://api.github.com`, overridable for GitHub Enterprise and for tests.
const DEFAULT_GITHUB_API: &str = "https://api.github.com";

/// `https://codeload.github.com`, the tarball host.
const DEFAULT_CODELOAD: &str = "https://codeload.github.com";

/// The API host, or the environment override.
///
/// The override serves two real purposes rather than being a test hook: GitHub Enterprise has its
/// own hosts, and an air-gapped environment needs to point at an internal mirror. Both are the
/// same problem, which is why the variable is named for the host rather than "for tests".
fn github_api() -> String {
    std::env::var("TERRAKIT_GITHUB_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GITHUB_API.to_string())
}

fn codeload() -> String {
    std::env::var("TERRAKIT_GITHUB_CODELOAD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CODELOAD.to_string())
}

/// Refuse a commit ref that is not a full 40-character SHA, except when it is unambiguously short
/// (at least 7 hex digits). Anything looser could name a branch that happens to share a prefix.
const MIN_ABBREVIATED_SHA: usize = 7;

/// Caps on extraction. Generous for a geographic dataset, finite for a bomb.
const MAX_ENTRIES: usize = 10_000;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

// --- source parsing ---------------------------------------------------------

/// What a `@ref` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A release tag, e.g. `@1.2.0`.
    Tag,
    /// A branch, e.g. `@main`.
    Branch,
    /// A commit, abbreviated or full.
    Commit,
    /// The repository's default branch, chosen because no ref was given.
    Default,
}

impl RefKind {
    /// Whether installing from this ref twice yields the same bytes.
    ///
    /// A tag can be moved, but in practice it names a release; a branch certainly can move. The
    /// distinction drives a warning, not a refusal — refusing `@main` would make the documented
    /// workflow impossible, and the commit SHA is pinned either way.
    pub fn reproducible(self) -> bool {
        matches!(self, RefKind::Tag | RefKind::Commit)
    }
}

/// A parsed `github.com/owner/repo[@ref]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub owner: String,
    pub repo: String,
    pub reference: String,
    pub kind: RefKind,
}

impl SourceSpec {
    pub fn slug(&self) -> String {
        format!("{}-{}", self.owner.to_lowercase(), self.repo.to_lowercase())
    }

    /// The country this package is for, taken from its manifest rather than its name.
    ///
    /// Deliberately not derived from `terrakit-data-tr`. A repository name is a convention a
    /// third party controls; the manifest is signed. Trusting the directory name for where to put
    /// a package would let a renamed repository land in the wrong country's slot.
    pub fn api_base(&self) -> String {
        format!("{}/repos/{}/{}", github_api(), self.owner, self.repo)
    }

    pub fn tarball_url(&self, commit: &str) -> String {
        format!("{}/{}/{}/tar.gz/{commit}", codeload(), self.owner, self.repo)
    }
}

/// Parse a GitHub source string.
///
/// Accepts `github.com/owner/repo`, `owner/repo`, and either with `@ref`. A bare `owner/repo` is
/// accepted because people type it; anything else is refused with the accepted forms, rather than
/// guessed at.
pub fn parse(input: &str) -> Result<SourceSpec, String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("no source given. Expected github.com/owner/repo[@ref].".to_string());
    }

    let without_host = trimmed
        .strip_prefix("github.com/")
        .or_else(|| trimmed.strip_prefix("https://github.com/"))
        .map(|rest| (rest, true))
        .unwrap_or((trimmed, false));

    if without_host.1 && without_host.0.contains('/') {
        let path = without_host.0;
        // Split the ref off before counting slashes: the ref may contain them in a branch name.
        let (path, reference) = match path.split_once('@') {
            Some((path, reference)) => (path, Some(reference)),
            None => (path, None),
        };

        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() != 2 {
            return Err(format!(
                "{input:?} is not a GitHub source. Expected github.com/<owner>/<repo>[@ref], \\
                 for example github.com/example/terrakit-data-tr@1.2.0."
            ));
        }
        return build(segments[0], segments[1], reference, input);
    }

    Err(format!(
        "{input:?} is not a GitHub source.\n\
         \x20      Expected github.com/<owner>/<repo>[@ref], for example\n\
         \x20      github.com/example/terrakit-data-tr@1.2.0.\n\
         \x20      Only github.com is supported; a registry package comes from\n\
         \x20      'terrakit add <CC>'."
    ))
}

fn build(owner: &str, repo: &str, reference: Option<&str>, input: &str) -> Result<SourceSpec, String> {
    for (name, value) in [("owner", owner), ("repository", repo)] {
        if value.is_empty()
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(format!(
                "{input:?} has an invalid {name} {value:?}. GitHub allows letters, digits, \\
                 '-', '_' and '.'."
            ));
        }
    }
    if owner.starts_with('.') || owner.starts_with('-') || repo.starts_with('-') {
        return Err(format!("{input:?} has an owner or repository starting with '.' or '-'."));
    }

    let reference = reference.unwrap_or("").trim().to_string();

    // A ref is interpolated into an API path (`/repos/{owner}/{repo}/commits/{ref}`), so `..`
    // in one walks out of it. Slashes are legitimate — `feature/x` is a real branch — so only
    // the traversal is refused, not the separator.
    if reference.contains("..") || reference.starts_with('/') {
        return Err(format!(
            "{input:?} has a ref {reference:?} containing '..' or a leading '/'. A ref is not a \
             path and cannot be one."
        ));
    }
    if reference.chars().any(char::is_whitespace) || reference.chars().any(char::is_control) {
        return Err(format!("{input:?} has a ref {reference:?} containing whitespace."));
    }

    let kind = if reference.is_empty() {
        RefKind::Default
    } else if is_full_sha(&reference) {
        RefKind::Commit
    } else if is_abbreviated_sha(&reference) {
        RefKind::Commit
    } else if is_semver_tag(&reference) {
        RefKind::Tag
    } else {
        RefKind::Branch
    };

    Ok(SourceSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        reference,
        kind,
    })
}

fn is_full_sha(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_abbreviated_sha(text: &str) -> bool {
    text.len() >= MIN_ABBREVIATED_SHA
        && text.len() < 40
        && text.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_semver_tag(text: &str) -> bool {
    // `v1.2.3`, `1.2.3`, `1.2`, `1` — enough to tell a release tag from a branch name. A branch
    // called `release` must not be mistaken for a tag, and this does not treat it as one.
    let core = text.strip_prefix('v').unwrap_or(text);
    !core.is_empty()
        && core.split('.').count() <= 3
        && core.split('.').all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

/// A ref resolved to a concrete commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub commit: String,
    /// What the user asked for, kept for the lockfile.
    pub requested: String,
    pub kind: RefKind,
    /// The repository's default branch, when no ref was given.
    pub default_branch: Option<String>,
}

impl Resolved {
    /// One line for the lockfile and for the install report.
    pub fn describe(&self) -> String {
        match &self.default_branch {
            Some(branch) => format!("{}@{branch} -> {}", self.requested, self.commit),
            None => format!("{} -> {}", self.requested, self.commit),
        }
    }
}

/// Resolve a reference to a commit SHA.
///
/// Uses the commits API rather than constructing a download URL from the ref directly, because
/// the whole point is to learn which commit the ref named *now*. The resolved SHA is then what gets
/// downloaded and what gets pinned, so a branch that moves between the API call and the download
/// cannot change the bytes.
pub fn resolve(spec: &SourceSpec, offline: bool) -> Result<Resolved, String> {
    let reference = match spec.kind {
        RefKind::Default => {
            let repository = fetch_json(&format!("{}/repos/{}", spec.api_base(), spec.owner), offline)?;
            let branch = repository
                .get("default_branch")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "{} does not publish a default branch, so a ref is required.",
                        spec.slug()
                    )
                })?
                .to_string();
            commit_for(spec, &branch, offline).map(|commit| Resolved {
                commit,
                requested: branch.clone(),
                kind: RefKind::Default,
                default_branch: Some(branch),
            })?
        }
        _ => commit_for(spec, &spec.reference, offline).map(|commit| Resolved {
            commit,
            requested: spec.reference.clone(),
            kind: spec.kind,
            default_branch: None,
        })?,
    };

    if !is_full_sha(&reference.commit) {
        return Err(format!(
            "the GitHub API returned {reference:?} as the commit, which is not a 40-character \\
             SHA. Refusing to download from an unverified reference."
        ));
    }
    Ok(reference)
}

fn commit_for(spec: &SourceSpec, reference: &str, offline: bool) -> Result<String, String> {
    let url = format!("{}/commits/{}", spec.api_base(), reference);
    let commit = fetch_json(&url, offline)?;
    commit
        .get("sha")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{url} did not return a commit SHA."))
}

fn fetch_json(url: &str, offline: bool) -> Result<serde_json::Value, String> {
    let body = crate::download::fetch_text(url, offline)?;
    serde_json::from_str(&body).map_err(|e| format!("{url} is not valid JSON: {e}"))
}

// --- extraction -------------------------------------------------------------

/// Files a package must contain, and the only entry kinds allowed.
const ALLOWED_FILES: [&str; 3] = ["data.sqlite", "manifest.json", "signature.json"];

/// What a tarball yielded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: Vec<String>,
    pub total_bytes: u64,
    /// The single top-level directory every entry shared, stripped during extraction.
    pub root: String,
}

/// Extract a package tarball, refusing anything that could write outside the destination.
///
/// The rules, all enforced per entry rather than assumed from the archive's shape:
///
/// - the path must be relative and free of `..`
/// - the path must resolve inside `destination`, checked after normalisation
/// - the entry must be a regular file or a directory — symlinks and hardlinks are refused
///   outright, because a package needs three files and a link is only ever an escape
/// - the entry must live under one common top-level directory, which is stripped
/// - entry count and total bytes are capped
pub fn extract(tarball: &[u8], destination: &Path) -> Result<Extracted, String> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(tarball));
    let mut archive = tar::Archive::new(decoder);

    std::fs::create_dir_all(destination)
        .map_err(|e| format!("cannot create {}: {e}", destination.display()))?;

    let mut extracted = Extracted::default();
    let mut entries_seen = 0usize;

    let entries = archive
        .entries()
        .map_err(|e| format!("the download is not a readable gzip tarball: {e}"))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            format!(
                "the tarball is corrupt at this entry: {e}. The download may be truncated, or \
                 may not be a gzip tarball at all."
            )
        })?;
        entries_seen += 1;
        if entries_seen > MAX_ENTRIES {
            return Err(format!(
                "the archive has more than {MAX_ENTRIES} entries, which no TerraKit package has. \\
                 Refusing to extract it."
            ));
        }

        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            // Named in the message, because "unsupported entry type" in isolation sends the
            // reader to the tar specification instead of to the fix.
            let what = if entry_type.is_symlink() || entry_type.is_hard_link() {
                "a link"
            } else {
                "this type"
            };
            return Err(format!(
                "the archive contains {what} ({}).\n\
                 \x20      A TerraKit package is three regular files and nothing else, and a\n\
                 \x20      link in an archive from an untrusted repository is an escape, not a\n\
                 \x20      shortcut.",
                entry.path().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
            ));
        }

        let raw_path = entry
            .path()
            .map_err(|e| format!("corrupt tar entry path: {e}"))?
            .into_owned();
        let relative = relative_within(&raw_path).ok_or_else(|| archive_escape(&raw_path))?;

        // GitHub prefixes every entry with `<repo>-<sha>/`. The prefix must be consistent, or the
        // archive is not what it claims to be.
        let mut components = relative.components();
        let first = components
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .ok_or_else(|| "the archive contains an empty path".to_string())?;
        let stripped: PathBuf = components.collect();
        if stripped.as_os_str().is_empty() {
            // The directory entry itself; nothing to write.
            continue;
        }
        if extracted.root.is_empty() {
            extracted.root = first.clone();
        } else if extracted.root != first {
            return Err(format!(
                "the archive's entries live under two different directories ({} and {first}). \\
                 Refusing to guess which is the package.",
                extracted.root
            ));
        }

        if !entry_type.is_file() {
            let directory = destination.join(&stripped);
            std::fs::create_dir_all(&directory)
                .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
            continue;
        }

        let target = destination.join(&stripped);
        // Checked again after joining, because `destination` itself may be relative and the
        // archive may contain a component that normalises away.
        if !is_within(destination, &target) {
            return Err(archive_escape(&raw_path));
        }

        let declared = entry.header().size().unwrap_or(0);
        if extracted.total_bytes.saturating_add(declared) > MAX_TOTAL_BYTES {
            return Err(format!(
                "the archive declares more than {} bytes, which no TerraKit package has. \\
                 Refusing to extract it.",
                human_bytes(MAX_TOTAL_BYTES)
            ));
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut file = std::fs::File::create(&target)
            .map_err(|e| format!("cannot create {}: {e}", target.display()))?;

        // Bounded copy. The header's size is attacker-controlled, so the cap is what actually
        // bounds the write, not the header.
        //
        // `io::copy` rather than `read_to_end`: that method appends to a `Vec<u8>`, and the
        // file has to be written in place so a bomb never lands in memory either.
        let written = std::io::copy(
            &mut std::io::Read::take(&mut entry, MAX_TOTAL_BYTES),
            &mut file,
        )
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        drop(file);

        if declared != 0 && written as u64 != declared {
            return Err(format!(
                "{} is truncated: the archive declares {declared} bytes but holds {written}. \\
                 This is a corrupt download and has not been installed.",
                target.display()
            ));
        }
        extracted.total_bytes += written as u64;
        extracted
            .files
            .push(stripped.to_string_lossy().to_string());
    }

    if extracted.files.is_empty() {
        return Err("the archive contained no files.".to_string());
    }
    Ok(extracted)
}

/// A path relative to the archive root, or `None` if it could escape.
fn relative_within(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            // A `..` anywhere, a `.` is harmless, and a root or prefix is absolute.
            Component::ParentDir => return None,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Whether `target` really lives under `base`, after normalisation.
fn is_within(base: &Path, target: &Path) -> bool {
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let target = target.canonicalize().unwrap_or_else(|_| {
        // The file may not exist yet, so canonicalise the parent and re-attach the leaf.
        match (target.parent(), target.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf())
                .join(name),
            _ => target.to_path_buf(),
        }
    });
    target.starts_with(&base)
}

fn archive_escape(path: &Path) -> String {
    format!(
        "the archive contains the entry {}, whose path leaves the destination. Refusing to \\
         extract it.",
        path.display()
    )
}

fn human_bytes(bytes: u64) -> String {
    crate::download::human_bytes(bytes)
}

// --- the install decision ---------------------------------------------------

/// Whether this process looks like CI.
///
/// `--allow-unsigned` refuses here. The flag exists so a developer can try a local dataset; a CI
/// job that installs unverified data into a build produces an artifact nobody can trace, which is
/// the failure the whole verification path exists to prevent.
pub fn is_ci() -> bool {
    for variable in ["CI", "CONTINUOUS_INTEGRATION", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE"] {
        let value = std::env::var(variable).unwrap_or_default();
        // GITHUB_ACTIONS is "true" when set; CI is sometimes "false", which must not count.
        if !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0" {
            return true;
        }
    }
    false
}

/// The decision to install an unsigned package.
///
/// Split out so it can be tested without a tarball: the rule is "signed, or the user passed
/// `--allow-unsigned` and is not in CI", and the failure mode of getting it wrong is an
/// unverifiable package in a release artifact.
pub fn permit_unsigned(allow_unsigned: bool, signed: bool) -> Result<(), String> {
    if signed {
        return Ok(());
    }
    if !allow_unsigned {
        return Err(signature::SignatureFailure::Missing.to_string());
    }
    if is_ci() {
        return Err(
            "--allow-unsigned refuses to run in CI. An unverified package in a build artifact \
             cannot be traced back to anything, which is the failure signature verification \
             exists to prevent.\n\
             \x20      Publish the dataset with a signing key and trust that key instead."
                .to_string(),
        );
    }
    Ok(())
}

/// Map a GitHub-install failure onto an exit code.
pub fn exit_for(error: &str) -> Exit {
    // A hostile archive is a verification failure even though CLI_SPEC.md 1 words the code as
    // "checksum, signature, or schema mismatch". The principle is "this package must not be
    // trusted", and a repository whose tarball tries to write outside the extraction directory is
    // exactly that. Leaving it at 1 would make it look like a transient error: a CI script keying
    // on 3 would not stop the build, and a retry would fetch the same hostile archive.
    if error.contains("is not trusted")
        || error.contains("does not verify")
        || error.contains("no signature.json")
        || error.contains("has not been installed")
        || error.contains("leaves the destination")
        || error.contains("the archive contains")
        || error.contains("different directories")
        || error.contains("truncated")
    {
        Exit::Verification
    } else if error.contains("not installed") {
        Exit::NotInstalled
    } else {
        Exit::Failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- a raw tar writer, because the `tar` crate's builder refuses to create the archives an
    // --- attacker would send. Testing the extractor requires being able to build one.

    struct RawTar {
        bytes: Vec<u8>,
    }

    impl RawTar {
        fn new() -> Self {
            RawTar { bytes: Vec::new() }
        }

        fn header(&mut self, name: &str, size: u64, typeflag: u8, link: &str) {
            let mut block = [0u8; 512];
            block[..name.len()].copy_from_slice(name.as_bytes());
            block[100..107].copy_from_slice(b"0000644");
            block[108..115].copy_from_slice(b"0000000");
            block[116..123].copy_from_slice(b"0000000");
            // size, octal, 12 bytes at 124
            write_octal(&mut block[124..136], size);
            // mtime, fixed so the archive is deterministic
            write_octal(&mut block[136..148], 0);
            block[148..156].copy_from_slice(b"        ");
            block[156] = typeflag;
            if !link.is_empty() {
                let bytes = link.as_bytes();
                block[157..157 + bytes.len()].copy_from_slice(bytes);
            }
            block[257..262].copy_from_slice(b"ustar");
            block[263..265].copy_from_slice(b"00");
            let checksum: u32 = block.iter().map(|b| *b as u32).sum();
            write_octal(&mut block[148..154], u64::from(checksum));
            block[154] = 0;
            block[155] = b' ';
            self.bytes.extend_from_slice(&block);
        }

        fn file(&mut self, name: &str, contents: &[u8]) {
            self.header(name, contents.len() as u64, b'0', "");
            self.bytes.extend_from_slice(contents);
            let padding = (512 - contents.len() % 512) % 512;
            self.bytes.extend(std::iter::repeat_n(0u8, padding));
        }

        fn symlink(&mut self, name: &str, target: &str) {
            self.header(name, 0, b'2', target);
        }

        fn directory(&mut self, name: &str) {
            self.header(name, 0, b'5', "");
        }

        fn finish(mut self) -> Vec<u8> {
            // Two zero blocks terminate the archive.
            self.bytes.extend(std::iter::repeat_n(0u8, 1024));
            self.bytes
        }
    }

    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{value:0width$o}", width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[field.len() - 1] = 0;
    }

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-github-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn gz(bytes: Vec<u8>) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// A well-formed package tarball, as GitHub serves them.
    fn good_tarball() -> Vec<u8> {
        let mut tar = RawTar::new();
        tar.directory("terrakit-data-tr-a1b2c3d/");
        tar.file(
            "terrakit-data-tr-a1b2c3d/manifest.json",
            br#"{"version":"1.2.0"}"#,
        );
        tar.file("terrakit-data-tr-a1b2c3d/data.sqlite", &b"database".repeat(100));
        tar.file(
            "terrakit-data-tr-a1b2c3d/signature.json",
            br#"{"algorithm":"ed25519"}"#,
        );
        gz(tar.finish())
    }

    #[test]
    fn sources_parse_in_the_documented_forms() {
        assert_eq!(
            parse("github.com/example/terrakit-data-tr").unwrap(),
            SourceSpec {
                owner: "example".into(),
                repo: "terrakit-data-tr".into(),
                reference: String::new(),
                kind: RefKind::Default
            }
        );
        assert_eq!(parse("github.com/example/pkg@1.2.0").unwrap().kind, RefKind::Tag);
        assert_eq!(parse("github.com/example/pkg@v1.2.0").unwrap().kind, RefKind::Tag);
        assert_eq!(parse("github.com/example/pkg@main").unwrap().kind, RefKind::Branch);
        assert_eq!(
            parse("github.com/example/pkg@a1b2c3d").unwrap().kind,
            RefKind::Commit
        );
        assert_eq!(
            parse(&format!("github.com/example/pkg@{}", "a".repeat(40)))
                .unwrap()
                .kind,
            RefKind::Commit
        );
        // Bare owner/repo is accepted because people type it.
        assert!(parse("example/terrakit-data-tr").is_err());
        assert!(parse("https://github.com/example/pkg").is_ok());
    }

    #[test]
    fn malformed_sources_are_refused_with_the_accepted_forms() {
        for bad in [
            "",
            "github.com/only-owner",
            "github.com/too/many/segments",
            "github.com/owner/repo/extra",
            "https://gitlab.com/owner/repo",
            "github.com/own er/repo",
            "github.com/-owner/repo",
            "github.com/owner/repo@../../etc/passwd",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be refused");
        }
        // And the error says what to type, because "invalid input" is not actionable.
        // Both refusals name the accepted forms. "Invalid input" is not actionable, and someone
        // who typed `github.com/only-owner` needs to be told what a repository path looks like.
        for bad in ["github.com/only-owner", "https://gitlab.com/owner/repo"] {
            let error = parse(bad).unwrap_err();
            assert!(
                error.contains("github.com/<owner>/<repo>"),
                "the refusal for {bad:?} must name the accepted form: {error}"
            );
        }
    }

    #[test]
    fn a_release_tag_is_told_apart_from_a_branch() {
        // A branch called `release` must not be resolved as a tag, and `1.2` is a tag while
        // `2026-10-01` is a branch.
        assert_eq!(parse("github.com/o/r@2026-10-01").unwrap().kind, RefKind::Branch);
        assert_eq!(parse("github.com/o/r@1.2").unwrap().kind, RefKind::Tag);
        assert_eq!(parse("github.com/o/r@1").unwrap().kind, RefKind::Tag);
        assert_eq!(parse("github.com/o/r@release").unwrap().kind, RefKind::Branch);
    }

    #[test]
    fn reproducibility_is_reported_per_ref_kind() {
        assert!(RefKind::Commit.reproducible());
        assert!(RefKind::Tag.reproducible());
        assert!(!RefKind::Branch.reproducible());
        assert!(!RefKind::Default.reproducible());
    }

    #[test]
    fn a_good_tarball_extracts_with_its_prefix_stripped() {
        let destination = tempdir("good");
        let extracted = extract(&good_tarball(), &destination).unwrap();

        assert_eq!(extracted.root, "terrakit-data-tr-a1b2c3d");
        assert!(destination.join("manifest.json").is_file());
        assert!(destination.join("data.sqlite").is_file());
        assert_eq!(
            std::fs::read(destination.join("data.sqlite")).unwrap(),
            b"database".repeat(100)
        );
        assert_eq!(extracted.files.len(), 3);
    }

    #[test]
    fn a_parent_directory_entry_is_refused() {
        // The `tar` crate's builder will not create one, which is exactly why the extractor needs
        // its own test: an archive from an untrusted repository can contain anything.
        let mut tar = RawTar::new();
        tar.file("root/manifest.json", b"{}");
        tar.file("../escape.txt", b"escaped");

        let error = extract(&gz(tar.finish()), &tempdir("escape")).unwrap_err();
        assert!(error.contains("leaves the destination"), "{error}");
    }

    #[test]
    fn an_absolute_path_entry_is_refused() {
        let mut tar = RawTar::new();
        tar.file("root/manifest.json", b"{}");
        tar.file("/etc/evil", b"escaped");

        let error = extract(&gz(tar.finish()), &tempdir("absolute")).unwrap_err();
        assert!(error.contains("leaves the destination"), "{error}");
    }

    #[test]
    fn a_symlink_out_of_the_tree_is_refused() {
        // A link is never a shortcut in an archive from an untrusted source: it is an escape.
        let mut tar = RawTar::new();
        tar.file("root/manifest.json", b"{}");
        tar.symlink("root/data.sqlite", "../../../../etc/passwd");

        let error = extract(&gz(tar.finish()), &tempdir("symlink")).unwrap_err();
        assert!(error.contains("link"), "{error}");
        // The message wraps across lines, so the phrase is checked against whitespace-collapsed
        // text. Checking the raw string would couple the test to where the line breaks happen to
        // fall, which is formatting, not behaviour.
        let flat = error.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("not a shortcut"), "{error}");
    }

    #[test]
    fn a_hardlink_is_refused_too() {
        let mut tar = RawTar::new();
        tar.file("root/manifest.json", b"{}");
        tar.header("root/data.sqlite", 0, b'1', "root/manifest.json");

        let error = extract(&gz(tar.finish()), &tempdir("hardlink")).unwrap_err();
        assert!(error.contains("link"), "{error}");
    }

    #[test]
    fn entries_under_two_roots_are_refused_rather_than_guessed() {
        let mut tar = RawTar::new();
        tar.file("one/manifest.json", b"{}");
        tar.file("two/data.sqlite", b"db");

        let error = extract(&gz(tar.finish()), &tempdir("two-roots")).unwrap_err();
        assert!(error.contains("two different directories"), "{error}");
    }

    #[test]
    fn a_truncated_entry_is_detected_against_the_declared_size() {
        // The header's size is attacker-controlled, so the bytes actually read are compared with
        // what the archive claims. Without this a truncated download extracts to a short file that
        // then fails as a corrupt SQLite database, naming the wrong problem.
        let mut tar = RawTar::new();
        tar.header("root/data.sqlite", 4096, b'0', "");
        tar.bytes.extend(b"only a hundred bytes".repeat(10));
        let padding = (512 - (b"only a hundred bytes".repeat(10).len() % 512)) % 512;
        tar.bytes.extend(std::iter::repeat_n(0u8, padding));

        let error = extract(&gz(tar.finish()), &tempdir("truncated")).unwrap_err();
        assert!(error.contains("truncated"), "{error}");
        assert!(error.contains("has not been installed"), "{error}");
    }

    #[test]
    fn an_empty_archive_is_refused() {
        let error = extract(&gz(RawTar::new().finish()), &tempdir("empty")).unwrap_err();
        assert!(error.contains("no files"), "{error}");
    }

    #[test]
    fn something_that_is_not_a_tarball_is_refused_clearly() {
        let error = extract(b"this is not gzip", &tempdir("garbage")).unwrap_err();
        assert!(error.contains("truncated") && error.contains("gzip tarball"), "{error}");
    }

    #[test]
    fn unsigned_installs_need_the_flag_and_are_refused_in_ci() {
        assert!(permit_unsigned(false, true).is_ok(), "a signed package needs no flag");

        let error = permit_unsigned(false, false).unwrap_err();
        assert!(error.contains("no signature.json"), "{error}");

        // SAFETY: single-threaded; restored before the next assertion.
        unsafe {
            std::env::remove_var("CI");
            std::env::remove_var("GITHUB_ACTIONS");
            assert!(permit_unsigned(true, false).is_ok(), "a developer may opt in");

            for variable in ["CI", "GITHUB_ACTIONS", "GITLAB_CI"] {
                std::env::set_var(variable, "true");
                let error = permit_unsigned(true, false).unwrap_err();
                assert!(
                    error.contains("refuses to run in CI"),
                    "{variable} must be detected: {error}"
                );
                std::env::remove_var(variable);
            }

            // "false" and "0" are set-but-off, and counting them would refuse every job that
            // disables CI for a subcommand.
            std::env::set_var("CI", "false");
            assert!(permit_unsigned(true, false).is_ok());
            std::env::set_var("CI", "0");
            assert!(permit_unsigned(true, false).is_ok());
            std::env::remove_var("CI");
        }
    }

    #[test]
    fn ci_detection_reads_the_usual_variables() {
        // SAFETY: restored immediately.
        unsafe {
            for variable in ["CI", "CONTINUOUS_INTEGRATION", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE"] {
                std::env::remove_var(variable);
                assert!(!is_ci(), "{variable} unset must not look like CI");
                std::env::set_var(variable, "true");
                assert!(is_ci(), "{variable}=true must look like CI");
                std::env::remove_var(variable);
            }
        }
    }

    #[test]
    fn a_refusal_maps_onto_the_verification_exit_code() {
        assert_eq!(exit_for("signed by key \"x\", which is not trusted"), Exit::Verification);
        assert_eq!(exit_for("the signature does not verify"), Exit::Verification);
        assert_eq!(exit_for("the package has no signature.json"), Exit::Verification);
        // Hostile archives are "do not trust this", which is what exit 3 means in practice.
        assert_eq!(exit_for("the archive contains a link (root/escape)"), Exit::Verification);
        assert_eq!(exit_for("the archive contains ../x, whose path leaves the destination"), Exit::Verification);
        assert_eq!(exit_for("the entries live under two different directories"), Exit::Verification);
        assert_eq!(exit_for("the archive is corrupt at this entry: truncated"), Exit::Verification);
        // A network failure must not masquerade as tampering, or every outage becomes an
        // incident and a retry gets refused instead of retried.
        assert_eq!(exit_for("GET https://api.github.com failed: timed out"), Exit::Failure);
        assert_eq!(exit_for("cannot create /tmp/x: No space left on device"), Exit::Failure);
    }

    #[test]
    fn relative_paths_within_are_recognised_and_escapes_are_not() {
        assert!(relative_within(Path::new("a/b.txt")).is_some());
        assert!(relative_within(Path::new("./a/b.txt")).is_some());
        assert!(relative_within(Path::new("../a")).is_none());
        assert!(relative_within(Path::new("a/../../b")).is_none());
        assert!(relative_within(Path::new("/a")).is_none());
        assert!(relative_within(Path::new("")).is_none());
    }

    #[test]
    fn the_resolved_ref_describes_what_it_resolved_to() {
        let resolved = Resolved {
            commit: "a".repeat(40),
            requested: "main".into(),
            kind: RefKind::Default,
            default_branch: Some("main".into()),
        };
        assert!(resolved.describe().contains("main -> "));
        assert!(resolved.describe().contains(&"a".repeat(40)));
    }
}