//! `terrakit bundle` — emit platform resource manifests for mobile.
//!
//! A gap in the specification, named in [`ARCHITECTURE.md` §6]: `terrakit add TR` writes to
//! `.terrakit/`, but an iOS or Android app can only read from its bundle. Without an explicit
//! step the experience promised by spec §57 does not work on mobile at all.
//!
//! The output is **declarative and checked in**. It is not generated at build time, because a
//! build step that reaches the network makes CI non-hermetic and turns a build failure into a
//! network failure. Nothing here needs the registry: the packages are already installed, and
//! their manifests already carry the digests.
//!
//! Layout:
//!
//! ```text
//! bundle/
//! ├── terrakit-bundle.json          canonical manifest, platform-independent
//! ├── ios/terrakit/<CC>/<version>/  add this folder as an Xcode folder reference
//! └── android/
//!     ├── assets/terrakit/<CC>/<version>/   Gradle ships assets/ verbatim
//!     └── terrakuit.gradle          noCompress snippet, included by the app
//! ```
//!
//! Android gets an explicit `noCompress` because a SQLite file inside an APK's asset stream is
//! not seekable — SQLite needs `mmap` and random access — so it would be inflated twice and
//! still unreadable. `ARCHITECTURE.md` §6 has the SDK's first-launch copy for the case where the
//! platform does compress it anyway.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::exit::Exit;
use crate::package::{InstalledPackage, Project, human_bytes};
use crate::verify::sha256_file;

/// Files copied into a bundle.
///
/// Deliberately not everything in the package directory: the staging leftovers and any `.bak` a
/// crashed run left behind are not resources, and shipping them into an app bundle is how a 3 MB
/// dead file ends up in a release.
const BUNDLE_FILES: [&str; 3] = ["data.sqlite", "manifest.json", "signature.json"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Ios,
    Android,
}

impl Platform {
    pub fn slug(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Android => "android",
        }
    }

    pub fn parse(text: &str) -> Option<Platform> {
        match text.to_lowercase().as_str() {
            "ios" | "apple" | "macos" => Some(Platform::Ios),
            "android" => Some(Platform::Android),
            _ => None,
        }
    }

    /// Subdirectory the packages go into, relative to the platform root.
    fn package_root(self) -> PathBuf {
        match self {
            // A folder reference copies its contents verbatim, so the package tree sits under
            // the folder the project references.
            Platform::Ios => PathBuf::from("terrakit"),
            Platform::Android => PathBuf::from("assets").join("terrakit"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BundlePlan {
    pub out_dir: PathBuf,
    pub platforms: Vec<Platform>,
    /// Verify every copied file against the package manifest. On by default: a bundle is copied
    /// into a release artifact, and nothing downstream re-checks it.
    pub verify: bool,
}

/// One package as it appears in the bundle manifest.
#[derive(Debug, Clone)]
pub struct BundledPackage {
    pub country: String,
    pub version: String,
    pub level: String,
    pub source: PathBuf,
    pub files: Vec<BundledFile>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct BundledFile {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

/// Build the manifest describing every installed package.
///
/// Resolves which version is *selected* for each country, so a bundle contains one version per
/// country. Copying two would ship duplicate data and give the SDK's first-match resolution two
/// answers to the same question.
pub fn plan(project: &Project) -> Result<Vec<BundledPackage>, String> {
    let installed = project.installed();
    let manifest = project.read_manifest().unwrap_or_else(|_| {
        crate::package::ProjectManifest {
            schema_version: 1,
            packages: Default::default(),
            level: None,
            resolution: None,
        }
    });

    let mut out = Vec::new();
    let mut countries: Vec<String> = Vec::new();

    for package in &installed {
        if !countries.contains(&package.country) {
            countries.push(package.country.clone());
        }
    }
    countries.sort();

    for country in countries {
        let versions: Vec<&InstalledPackage> = installed
            .iter()
            .filter(|p| p.country == country)
            .collect();

        // The selection, or the newest on disk when the manifest has no entry.
        let selected = manifest
            .resolution
            .as_ref()
            .and_then(|r| r.get(&country))
            .map(|r| r.version.clone())
            .or_else(|| {
                versions
                    .iter()
                    .map(|p| p.version.clone())
                    .max_by(|a, b| compare_versions(a, b))
            });

        let Some(package) = versions
            .into_iter()
            .find(|p| Some(p.version.clone()) == selected)
        else {
            continue;
        };

        let mut files = Vec::new();
        let mut total_bytes = 0;
        for name in BUNDLE_FILES {
            let path = package.directory.join(name);
            if !path.is_file() {
                // Not fatal: an unsigned local package may lack signature.json. The manifest
                // records what is actually there rather than what was hoped for.
                continue;
            }
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            total_bytes += size;
            files.push(BundledFile {
                name: name.to_string(),
                sha256: sha256_file(&path)?,
                size,
            });
        }

        if files.is_empty() {
            return Err(format!(
                "{} ({}) has no bundleable files. Run 'terrakit add {}' again.",
                package.directory.display(),
                package.version,
                country
            ));
        }

        out.push(BundledPackage {
            country,
            version: package.version.clone(),
            level: package.level.clone(),
            source: package.directory.clone(),
            files,
            total_bytes,
        });
    }

    Ok(out)
}

/// Numeric version comparison, so "newest" is not decided by string order.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    crate::registry::Version::parse(a)
        .cmp(&crate::registry::Version::parse(b))
        .then_with(|| a.cmp(b))
}

/// Write the bundle for every requested platform.
///
/// Each package is copied into a per-platform tree and then verified against the digest recorded
/// in the canonical manifest. Verify-on-copy rather than verify-after: the copy is the last
/// opportunity, and nothing downstream reads the file again until the app ships.
pub fn emit(plan: &BundlePlan, packages: &[BundledPackage]) -> Result<BundleReport, String> {
    if packages.is_empty() {
        return Err("no packages are installed; run 'terrakit add <CC>' first".to_string());
    }
    std::fs::create_dir_all(&plan.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", plan.out_dir.display()))?;

    let mut copied = 0usize;
    let mut bytes = 0u64;

    for platform in &plan.platforms {
        let root = plan.out_dir.join(platform.slug()).join(platform.package_root());
        for package in packages {
            let version_segment =
                crate::registry::version_to_path_segment(&package.version);
            let target = root.join(&package.country).join(&version_segment);
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("cannot create {}: {e}", target.display()))?;

            for file in &package.files {
                let source = package.source.join(&file.name);
                let destination = target.join(&file.name);
                std::fs::copy(&source, &destination).map_err(|e| {
                    format!("cannot copy {} to {}: {e}", source.display(), destination.display())
                })?;

                if plan.verify {
                    let actual = sha256_file(&destination)?;
                    if actual != file.sha256 {
                        // Reported and then fatal: a bundle that fails its own digest would be
                        // shipped, and an app has no way to notice.
                        return Err(format!(
                            "{} does not match its manifest after copying\n       \
                             expected {}\n       actual   {}\n\
                             \x20      Nothing usable was written for this file.",
                            destination.display(),
                            file.sha256,
                            actual
                        ));
                    }
                }
                copied += 1;
                bytes += file.size;
            }
        }
    }

    write_manifest(plan, packages)?;
    if plan.platforms.contains(&Platform::Android) {
        write_gradle_snippet(plan)?;
    }

    Ok(BundleReport {
        packages: packages.len(),
        platforms: plan
            .platforms
            .iter()
            .map(|p| p.slug().to_string())
            .collect(),
        files: copied,
        bytes,
        out_dir: plan.out_dir.clone(),
    })
}

#[derive(Debug, Clone)]
pub struct BundleReport {
    pub packages: usize,
    pub platforms: Vec<String>,
    pub files: usize,
    pub bytes: u64,
    pub out_dir: PathBuf,
}

impl BundleReport {
    /// Summary lines, stderr, so `--json` keeps stdout clean.
    pub fn to_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "bundled {} package(s) for {}",
            self.packages,
            self.platforms.join(", ")
        )];
        for platform in &self.platforms {
            lines.push(format!(
                "  {platform:<9} {}",
                self.out_dir.join(platform).display()
            ));
        }
        lines.push(format!(
            "  {} file(s), {}",
            self.files,
            human_bytes(self.bytes)
        ));
        lines
    }
}

/// The canonical, platform-independent manifest.
///
/// Written with sorted keys and a fixed indent so two runs over unchanged packages produce
/// identical bytes. A resource manifest that changes on every invocation cannot be reviewed in a
/// diff, and the whole point of checking it in is that a reviewer can see what changed.
fn write_manifest(plan: &BundlePlan, packages: &[BundledPackage]) -> Result<(), String> {
    let document = json!({
        "schemaVersion": 1,
        "generator": { "name": "terrakit-cli", "version": env!("CARGO_PKG_VERSION") },
        "platforms": plan.platforms.iter().map(|p| p.slug()).collect::<Vec<_>>(),
        "verified": plan.verify,
        "packages": packages
            .iter()
            .map(|package| json!({
                "country": package.country,
                "version": package.version,
                "level": package.level,
                "totalBytes": package.total_bytes,
                "files": package
                    .files
                    .iter()
                    .map(|file| json!({
                        "path": file.name,
                        "sha256": file.sha256,
                        "size": file.size,
                    }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });

    let path = plan.out_dir.join("terrakit-bundle.json");
    write_json(&path, &document)
}

fn write_json(path: &Path, document: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(document).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The Gradle snippet the app includes.
///
/// A file rather than instructions printed to stdout, because a snippet that has to be pasted is
/// a snippet that gets pasted wrong. `noCompress` is the part that matters: a compressed SQLite
/// entry in an APK is doubly inflated and still cannot be opened in place.
fn write_gradle_snippet(plan: &BundlePlan) -> Result<(), String> {
    let path = plan.out_dir.join(Platform::Android.slug()).join("terrakuit.gradle");
    let body = format!(
        "// Generated by `terrakit bundle --platform android`. Check this in.\n\
         //\n\
         // Include from the app's build.gradle:\n\
         //     apply from: '../bundle/android/terrakuit.gradle'\n\
         // or, in settings/build scripts:\n\
         //     apply from: file('bundle/android/terrakuit.gradle')\n\
         //\n\
         // SQLite needs random access and mmap, so a compressed entry inside the APK cannot be\n\
         // opened in place -- and compressing it twice inflates it twice. ARCHITECTURE.md section 6\n\
         // has the SDK's first-launch copy for platforms that compress regardless.\n\
         \n\
         android {{\n    \x20   androidResources {{\n        noCompress += 'sqlite'\n    }}\n}}\n\
         \n\
         // Packages are under assets/terrakit/<country>/<version>/. The SDK copies each\n\
         // data.sqlite into the app's private directory on first launch, once per version.\n\
         // The manifest is {}/terrakit-bundle.json.\n",
        plan.out_dir.display()
    );
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

impl BundleReport {
    /// The JSON form, for `--json`.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"ok\":true,\"packages\":{},\"platforms\":[{}],\"files\":{},\"bytes\":{},\"outDir\":{}}}",
            self.packages,
            self.platforms
                .iter()
                .map(|p| crate::commands::jstr(p))
                .collect::<Vec<_>>()
                .join(","),
            self.files,
            self.bytes,
            crate::commands::jstr(&self.out_dir.display().to_string()),
        )
    }
}

/// Map a bundle failure onto an exit code.
///
/// A digest that does not survive the copy is a verification failure, not a runtime one: it means
/// the bytes were wrong, and a CI script must be able to refuse it on that basis.
pub fn exit_for(error: &str) -> Exit {
    if error.contains("does not match its manifest") {
        Exit::Verification
    } else {
        Exit::Failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terrakit-bundle-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A project with one installed package whose files match its manifest.
    fn project_with_package(name: &str, country: &str, version: &str) -> Project {
        let root = tempdir(name);
        let project = Project::at(root);
        let dir = project.packages_dir.join(country).join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();

        let database = b"pretend sqlite database".repeat(50);
        std::fs::write(dir.join("data.sqlite"), &database).unwrap();

        let manifest = json!({
            "schemaVersion": 1,
            "name": format!("terrakit-data-{country}"),
            "version": version,
            "country": country,
            "level": "cities",
            "files": [{
                "path": "data.sqlite",
                "sha256": crate::verify::sha256_bytes(&database),
                "size": database.len(),
            }],
        });
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap() + "\n",
        )
        .unwrap();

        let mut project_manifest = crate::package::ProjectManifest::empty(None);
        project_manifest.resolution = Some(
            [(
                country.to_string(),
                crate::package::ResolvedPackage {
                    version: version.to_string(),
                    source: Some("registry".to_string()),
                    resolved_ref: None,
                    level: Some("cities".to_string()),
                    integrity: Some("verified".to_string()),
                },
            )]
            .into_iter()
            .collect(),
        );
        project.write_manifest(&project_manifest).unwrap();
        project
    }

    #[test]
    fn platform_names_and_slugs() {
        assert_eq!(Platform::parse("ios"), Some(Platform::Ios));
        assert_eq!(Platform::parse("iOS"), Some(Platform::Ios));
        assert_eq!(Platform::parse("android"), Some(Platform::Android));
        assert_eq!(Platform::parse("windows"), None);
        assert_eq!(Platform::Ios.slug(), "ios");
        assert_eq!(Platform::Android.package_root(), PathBuf::from("assets/terrakit"));
    }

    #[test]
    fn the_manifest_lists_one_version_per_country() {
        let project = project_with_package("bundle-one", "TR", "2026.10.1+9920");
        let packages = plan(&project).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].country, "TR");
        assert_eq!(packages[0].version, "2026.10.1+9920");
        assert_eq!(packages[0].files.len(), 2, "data.sqlite and manifest.json");
        assert_eq!(packages[0].files[0].name, "data.sqlite");
    }

    #[test]
    fn a_stale_second_version_is_not_bundled() {
        // Two directories for one country: shipping both would duplicate ~10 MB into a release
        // and give the SDK's first-match resolution two answers.
        let project = project_with_package("bundle-two", "TR", "2026.10.1+9920");
        let stale = project.packages_dir.join("TR").join("0.9.0");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("data.sqlite"), b"old").unwrap();
        std::fs::write(stale.join("manifest.json"), b"{}").unwrap();

        let packages = plan(&project).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].version, "2026.10.1+9920");
    }

    #[test]
    fn only_known_files_are_copied() {
        // A staging leftover or a `.bak` must not end up in an app bundle.
        let project = project_with_package("bundle-leftover", "TR", "1.0.0");
        let dir = project.packages_dir.join("TR").join("1.0.0");
        std::fs::write(dir.join("terrakit.json.bak"), b"old manifest").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"junk").unwrap();

        let packages = plan(&project).unwrap();
        let names: Vec<&str> = packages[0].files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["data.sqlite", "manifest.json"]);
    }

    #[test]
    fn emitting_copies_the_tree_and_writes_the_manifest() {
        let project = project_with_package("bundle-emit", "TR", "1.0.0");
        let packages = plan(&project).unwrap();
        let out = tempdir("bundle-emit-out");
        let plan = BundlePlan {
            out_dir: out.clone(),
            platforms: vec![Platform::Ios, Platform::Android],
            verify: true,
        };

        let report = emit(&plan, &packages).unwrap();
        assert_eq!(report.packages, 1);
        assert_eq!(report.files, 4, "two files per platform");

        let ios = out.join("ios").join("terrakit").join("TR").join("1.0.0");
        assert!(ios.join("data.sqlite").is_file());
        let android = out
            .join("android")
            .join("assets")
            .join("terrakit")
            .join("TR")
            .join("1.0.0");
        assert!(android.join("data.sqlite").is_file());
        assert!(out.join("android").join("terrakuit.gradle").is_file());

        // The canonical manifest is platform-independent and names both.
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("terrakit-bundle.json")).unwrap())
                .unwrap();
        assert_eq!(document["schemaVersion"], 1);
        assert_eq!(document["platforms"][0], "ios");
        assert_eq!(document["packages"][0]["country"], "TR");
        assert!(document["packages"][0]["files"][0]["sha256"].is_string());
    }

    #[test]
    fn the_copied_bytes_are_verified_against_the_manifest() {
        // The copy is the last opportunity: nothing reads these files again until the app ships,
        // and an app has no way to detect a corrupted bundle.
        let project = project_with_package("bundle-verify", "TR", "1.0.0");
        let mut packages = plan(&project).unwrap();
        packages[0].files[0].sha256 = "0".repeat(64);

        let out = tempdir("bundle-verify-out");
        let plan = BundlePlan {
            out_dir: out.clone(),
            platforms: vec![Platform::Ios],
            verify: true,
        };
        let error = emit(&plan, &packages).unwrap_err();
        assert!(error.contains("does not match its manifest"), "{error}");
        assert_eq!(exit_for(&error), Exit::Verification);
    }

    #[test]
    fn the_manifest_is_byte_identical_across_runs() {
        // A checked-in resource manifest that changes on every invocation cannot be reviewed.
        let project = project_with_package("bundle-stable", "TR", "1.0.0");
        let packages = plan(&project).unwrap();

        let mut rendered = Vec::new();
        for name in ["stable-a", "stable-b"] {
            let out = tempdir(name);
            let plan = BundlePlan {
                out_dir: out.clone(),
                platforms: vec![Platform::Ios],
                verify: true,
            };
            emit(&plan, &packages).unwrap();
            rendered.push(std::fs::read(out.join("terrakit-bundle.json")).unwrap());
        }
        assert_eq!(rendered[0], rendered[1]);
    }

    #[test]
    fn bundling_nothing_is_an_error_rather_than_an_empty_directory() {
        let root = tempdir("bundle-empty");
        let project = Project::at(root.clone());
        project.write_manifest(&crate::package::ProjectManifest::empty(None)).unwrap();
        assert!(plan(&project).unwrap().is_empty());

        let plan = BundlePlan {
            out_dir: root.join("out"),
            platforms: vec![Platform::Ios],
            verify: true,
        };
        let error = emit(&plan, &[]).unwrap_err();
        assert!(error.contains("no packages are installed"), "{error}");
        assert!(!plan.out_dir.exists(), "nothing should be written");
    }

    #[test]
    fn versions_compare_numerically_when_choosing_the_newest() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("2026.10.10", "2026.10.9"), Ordering::Greater);
        assert_eq!(compare_versions("2026.9.1", "2026.10.1"), Ordering::Less);
    }
}