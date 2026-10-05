//! Package discovery and metadata.
//!
//! `.terrakit.json` records intent, the lockfile records resolved state, and the
//! package directory holds the artifacts. Splitting them mirrors
//! `package-lock.json`: requested ranges are per-project, installed state is
//! shared, and the lockfile is committed so a team gets the same dataset on every
//! machine.
//!
//! Resolution order is `CLI_SPEC.md` §"package resolution" and matches the SDKs:
//! explicit configuration, then the application bundle, then `.terrakit/`.
//! The CLI only implements the last two; the bundle case belongs to the SDKs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exit::Exit;

/// Filename of the project manifest. Fixed by `PACKAGE_FORMAT.md` §9.
pub const PROJECT_MANIFEST: &str = "terrakit.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub schema_version: u32,
    /// Requested ranges, keyed by ISO 3166-1 alpha-2. Semver ranges over CalVer
    /// dataset versions, so `^2026.10.0` accepts `2026.10.1` and rejects
    /// `2026.11.0`.
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<BTreeMap<String, ResolvedPackage>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPackage {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// `verified` when the installed files match the recorded checksums.
    ///
    /// Present because tampering in `.terrakit/` must be visible from `list`
    /// without running a separate verification command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

impl ProjectManifest {
    pub fn empty(level: Option<&str>) -> Self {
        ProjectManifest {
            schema_version: 1,
            packages: BTreeMap::new(),
            level: level.map(str::to_string),
            resolution: None,
        }
    }
}

/// One installed package directory.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub country: String,
    pub version: String,
    pub level: String,
    pub directory: PathBuf,
    pub database: PathBuf,
    pub manifest_path: PathBuf,
}

impl InstalledPackage {
    /// Total size of the package directory, in bytes.
    pub fn size_on_disk(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            let mut total = 0;
            let Ok(entries) = std::fs::read_dir(dir) else {
                return 0;
            };
            for entry in entries.flatten() {
                match entry.metadata() {
                    Ok(meta) if meta.is_dir() => total += walk(&entry.path()),
                    Ok(meta) => total += meta.len(),
                    Err(_) => continue,
                }
            }
            total
        }
        walk(&self.directory)
    }
}

/// The project root and the packages directory under it.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub packages_dir: PathBuf,
    pub manifest_path: PathBuf,
}

impl Project {
    /// Discover the project by walking up from `start`, the way every JS
    /// toolchain does. Walking up is what makes `terrakit` work from a
    /// subdirectory of the app, which is where people actually run it.
    pub fn discover(start: &Path) -> Option<Project> {
        let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        loop {
            let manifest = current.join(PROJECT_MANIFEST);
            if manifest.is_file() {
                return Some(Project {
                    packages_dir: current.join(".terrakit").join("packages"),
                    manifest_path: manifest,
                    root: current,
                });
            }
            if !current.pop() {
                return None;
            }
        }
    }

    pub fn at(root: PathBuf) -> Project {
        Project {
            packages_dir: root.join(".terrakit").join("packages"),
            manifest_path: root.join(PROJECT_MANIFEST),
            root,
        }
    }

    pub fn read_manifest(&self) -> Result<ProjectManifest, String> {
        match std::fs::read_to_string(&self.manifest_path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| {
                format!(
                    "{} is not valid: {e}. Delete it and run 'terrakit init'.",
                    self.manifest_path.display()
                )
            }),
            Err(_) => Err(format!(
                "no {} found in {} or any parent directory. Run 'terrakit init' first.",
                PROJECT_MANIFEST,
                self.root.display()
            )),
        }
    }

    pub fn write_manifest(&self, manifest: &ProjectManifest) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(manifest)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&self.manifest_path, format!("{json}\n"))
    }

    /// Every installed package, sorted by country for stable output.
    ///
    /// Reads the directory rather than trusting the lockfile: a package present on
    /// disk but absent from the lockfile is exactly the state `list` needs to
    /// report, and a lockfile-only view would hide it.
    pub fn installed(&self) -> Vec<InstalledPackage> {
        let mut out = Vec::new();
        let Ok(countries) = std::fs::read_dir(&self.packages_dir) else {
            return out;
        };
        for country_entry in countries.flatten() {
            if !country_entry.path().is_dir() {
                continue;
            }
            let country = country_entry.file_name().to_string_lossy().to_string();
            let Ok(versions) = std::fs::read_dir(country_entry.path()) else {
                continue;
            };
            for version_entry in versions.flatten() {
                let directory = version_entry.path();
                if !directory.is_dir() {
                    continue;
                }
                let database = directory.join("data.sqlite");
                if !database.is_file() {
                    continue;
                }
                let manifest_path = directory.join("manifest.json");
                let (level, version) = read_package_manifest(&manifest_path)
                    .unwrap_or_else(|| {
                        (
                            "unknown".to_string(),
                            version_entry.file_name().to_string_lossy().to_string(),
                        )
                    });
                out.push(InstalledPackage {
                    country: country.clone(),
                    version,
                    level,
                    directory,
                    database,
                    manifest_path,
                });
            }
        }
        out.sort_by(|a, b| {
            a.country
                .cmp(&b.country)
                .then_with(|| a.version.cmp(&b.version))
        });
        out
    }

    pub fn packages_for(&self, country: &str) -> Vec<InstalledPackage> {
        let upper = country.to_uppercase();
        self.installed()
            .into_iter()
            .filter(|p| p.country == upper)
            .collect()
    }
}

fn read_package_manifest(path: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let level = value
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    Some((level, version))
}

/// Require a project, or explain how to make one.
pub fn require_project(start: &Path) -> Result<Project, Exit> {
    match Project::discover(start) {
        Some(project) => Ok(project),
        None => {
            eprintln!(
                "terrakit: no {PROJECT_MANIFEST} found in {} or any parent directory.",
                start.display()
            );
            eprintln!("       Run 'terrakit init' to create one.");
            Err(Exit::NotInstalled)
        }
    }
}

/// Open a package read-only.
///
/// `OpenFlags::SQLITE_OPEN_READ_ONLY` rather than `read_only(true)`: the latter
/// still allows SQLite to create a `-wal` sidecar or a hot journal if it believes
/// it needs to, which would mean the CLI mutating a file it is only supposed to
/// read. See `SECURITY.md` §6.
pub fn open_database(path: &Path) -> Result<rusqlite::Connection, Exit> {
    let uri = format!("file:{}?mode=ro", path.display());
    match rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(conn) => Ok(conn),
        Err(e) => {
            eprintln!(
                "terrakit: cannot open {}: {e}",
                path.display()
            );
            Err(Exit::Failure)
        }
    }
}

/// Human-readable byte size. Used by `list` and `doctor`.
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

/// Format an f64 for display without trailing noise.
pub fn human_coord(value: f64) -> String {
    format!("{value:.4}")
}

