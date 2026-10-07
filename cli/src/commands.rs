//! Command implementations.
//!
//! One module per command group, dispatched from `main.rs`. Human output goes to
//! stderr; only machine-readable output goes to stdout. That split is the reason
//! `terrakit add TR --json` is usable in a script while `terrakit add TR` shows a
//! progress bar in the same terminal — see `CLI_SPEC.md` §1.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::exit::{Exit, Outcome, failure, usage_error};
use crate::normalize::normalize;
use crate::package::{
    self, InstalledPackage, Project, human_bytes, human_coord, open_database,
};
use crate::verify;

use crate::download;
use crate::install;
use crate::registry::{self, VersionRange};
use crate::signature::{self, Config};
use crate::query::{self, SearchOptions};

/// Whether output should be machine-readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

/// Write a line to stdout. The only path to stdout in the CLI.
fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
}

/// Progress and status text, always on stderr.
fn progress(text: &str) {
    eprintln!("  {text}");
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Render a JSON string literal.
///
/// Public because `main.rs` builds the validate payload and both must escape
/// identically — two escaping implementations is how a JSON output becomes
/// unparseable in a script that trusts it.
pub fn jstr(input: &str) -> String {
    format!("\"{}\"", json_escape(input))
}

/// Render a JSON string or null.
fn jstr_opt(input: &Option<String>) -> String {
    match input {
        Some(v) => jstr(v),
        None => "null".to_string(),
    }
}

// --- version ---------------------------------------------------------------

pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn version(out_format: Format) -> Outcome {
    match out_format {
        Format::Json => emit(&format!(
            "{{\"name\":\"terrakit\",\"version\":{},\"rustc\":{},\"target\":{}}}",
            jstr(CLI_VERSION),
            jstr(option_env!("TERRAKIT_RUSTC_VERSION").unwrap_or("unknown")),
            jstr(std::env::consts::ARCH)
        )),
        Format::Human => {
            emit(&format!("terrakit {CLI_VERSION}"));
        }
    }
    Ok(())
}

// --- init ------------------------------------------------------------------

pub fn init(root: &Path, level: Option<&str>, force: bool, out_format: Format) -> Outcome {
    let project = Project::at(root.to_path_buf());

    if project.manifest_path.exists() && !force {
        return usage_error(format!(
            "{} already exists. Use --force to overwrite; the old file is kept as a .bak.",
            project.manifest_path.display()
        ));
    }

    if project.manifest_path.exists() {
        // Backed up rather than replaced. `init --force` is the command someone
        // runs when something is wrong, and silently deleting the manifest that
        // records what was installed would be the worst possible response.
        let backup = project.manifest_path.with_extension("json.bak");
        if let Err(e) = std::fs::copy(&project.manifest_path, &backup) {
            return failure(format!("cannot back up the existing manifest: {e}"));
        }
        progress(&format!(
            "existing manifest backed up to {}",
            backup.display()
        ));
    }

    let manifest = package::ProjectManifest::empty(level);
    if let Err(e) = std::fs::create_dir_all(&project.packages_dir) {
        return failure(format!(
            "cannot create {}: {e}",
            project.packages_dir.display()
        ));
    }
    if let Err(e) = project.write_manifest(&manifest) {
        return failure(format!("cannot write the manifest: {e}"));
    }

    match out_format {
        Format::Json => emit(&format!(
            "{{\"ok\":true,\"manifest\":{},\"level\":{}}}",
            jstr(&project.manifest_path.display().to_string()),
            jstr_opt(&manifest.level)
        )),
        Format::Human => {
            emit(&format!("Initialized TerraKit in {}", project.root.display()));
            emit(&format!("  {}", project.manifest_path.display()));
            progress("Next: terrakit add TR");
        }
    }
    Ok(())
}

// --- list ------------------------------------------------------------------

pub fn list(project: &Project, out_format: Format) -> Outcome {
    let packages = project.installed();
    let manifest = project.read_manifest().unwrap_or_else(|_| package::ProjectManifest::empty(None));

    match out_format {
        Format::Json => {
            let rows: Vec<String> = packages
                .iter()
                .map(|p| {
                    format!(
                        "{{\"country\":{},\"version\":{},\"level\":{},\"sizeBytes\":{},\"integrity\":{},\"path\":{}}}",
                        jstr(&p.country),
                        jstr(&p.version),
                        jstr(&p.level),
                        p.size_on_disk(),
                        jstr(
                            verify::verify_package(&p.directory)
                                .map(|v| v.summary())
                                .unwrap_or("unverifiable")
                        ),
                        jstr(&p.directory.display().to_string())
                    )
                })
                .collect();
            emit(&format!("{{\"packages\":[{}],\"count\":{}}}", rows.join(","), rows.len()));
        }
        Format::Human => {
            if packages.is_empty() {
                emit("No packages installed.");
                progress("Run 'terrakit add TR' to install one.");
                return Ok(());
            }
            emit(&format!(
                "  {:<4}  {:<12}  {:<14}  {:>9}  {}",
                "CC", "LEVEL", "VERSION", "SIZE", "INTEGRITY"
            ));
            for p in &packages {
                // The directory's OWN version, not the project's resolved one.
                //
                // `resolution` records which version the project selected, and printing it in
                // every row made two installed versions of one country list the same version
                // twice — a column whose only job is to tell them apart. `update` removes the
                // superseded version, but a user can hold two (an explicit `--version` install,
                // or a rollback) and `list` must describe the disk truthfully either way.
                let version = p.version.clone();
                let resolved = manifest
                    .resolution
                    .as_ref()
                    .and_then(|r| r.get(&p.country))
                    .map(|r| r.version.clone());
                // Checksum verification, not signature verification: this is what
                // catches a package tampered with locally, which a valid signature
                // does not. See verify.rs and SECURITY.md section 7.
                let integrity = verify::verify_package(&p.directory)
                    .map(|v| v.summary())
                    .unwrap_or("unverifiable");
                // Mark the version the project actually selected, so a rollback is visible
                // without a second command.
                let marker = match resolved.as_deref() {
                    Some(resolved) if resolved != version => " (not selected)",
                    Some(_) => " *",
                    None => "",
                };
                emit(&format!(
                    "  {:<4}  {:<12}  {:<14}  {:>9}  {}{marker}",
                    p.country,
                    p.level,
                    version,
                    human_bytes(p.size_on_disk()),
                    integrity
                ));
            }
            emit("");
            // Explain the marker. A column whose annotations are unlabelled is decoration.
            if packages.iter().any(|p| {
                manifest
                    .resolution
                    .as_ref()
                    .and_then(|r| r.get(&p.country))
                    .map(|r| r.version != p.version)
                    .unwrap_or(false)
            }) {
                emit("  *  the version terrakit.json selected");
                emit("     (not selected)  another version of the same country is also present");
            }
            emit(&format!(
                "{} package(s) in {}",
                packages.len(),
                project.packages_dir.display()
            ));
        }
    }
    Ok(())
}

// --- info ------------------------------------------------------------------

pub fn info(project: &Project, country: &str, out_format: Format) -> Outcome {
    let packages = project.packages_for(country);
    let Some(package) = packages.last() else {
        eprintln!("terrakit: {country} is not installed.");
        eprintln!("       Run 'terrakit add {country}'.");
        return Err(Exit::NotInstalled);
    };
    let conn = open_database(&package.database)?;
    let info = query::country_info(&conn, country)?;

    match out_format {
        Format::Json => emit(&format!(
            "{{\"iso2\":{},\"iso3\":{},\"numericCode\":{},\"name\":{},\"continent\":{},\
             \"capital\":{},\"currency\":{},\"phone\":{},\"tld\":{},\"drivingSide\":{},\
             \"latitude\":{},\"longitude\":{},\"timezones\":[{}],\"languages\":[{}],\
             \"places\":{},\"postalCodes\":{},\"placeNames\":{},\"alternateNames\":{},\
             \"nameLanguages\":{},\"datasetVersion\":{},\"level\":{}}}",
            jstr(&info.iso2),
            jstr(&info.iso3),
            jstr(&info.numeric_code),
            jstr(&info.name),
            jstr(&info.continent),
            jstr_opt(&info.capital),
            jstr_opt(&info.currency),
            jstr_opt(&info.phone),
            jstr_opt(&info.tld),
            jstr_opt(&info.driving_side),
            info.latitude.map(|v| v.to_string()).unwrap_or("null".into()),
            info.longitude.map(|v| v.to_string()).unwrap_or("null".into()),
            info.timezones.iter().map(|t| jstr(t)).collect::<Vec<_>>().join(","),
            info.languages.iter().map(|t| jstr(t)).collect::<Vec<_>>().join(","),
            info.places,
            info.postal_codes,
            info.place_names,
            info.alternate_names,
            info.languages_count,
            jstr(&package.version),
            jstr(&package.level),
        )),
        Format::Human => {
            emit(&format!(
                "{} ({}/{} /{})",
                info.name, info.iso2, info.iso3, info.numeric_code
            ));
            emit(&format!("    Continent    {}", info.continent));
            emit(&format!("    Capital      {}", info.capital.as_deref().unwrap_or("—")));
            emit(&format!("    Currency     {}", info.currency.as_deref().unwrap_or("—")));
            emit(&format!("    Phone        {}", info.phone.as_deref().unwrap_or("—")));
            emit(&format!("    TLD          {}", info.tld.as_deref().unwrap_or("—")));
            emit(&format!("    Driving      {}", info.driving_side.as_deref().unwrap_or("—")));
            emit(&format!(
                "    Timezone     {}",
                if info.timezones.is_empty() {
                    "—".to_string()
                } else {
                    info.timezones.join(", ")
                }
            ));
            emit(&format!(
                "    Languages    {}",
                if info.languages.is_empty() {
                    "—".to_string()
                } else {
                    info.languages.join(", ")
                }
            ));
            if let (Some(lat), Some(lon)) = (info.latitude, info.longitude) {
                emit(&format!("    Coordinates  {}, {}", human_coord(lat), human_coord(lon)));
            }
            emit("");
            emit(&format!("    Dataset      terrakit-data-{} {}", info.iso2.to_lowercase(), package.version));
            emit(&format!("    Level        {}", package.level));
            emit(&format!("    Places       {}", info.places));
            emit(&format!("    Postal       {} codes", info.postal_codes));
            emit(&format!(
                "    Names        {} localized in {} languages, {} alternates",
                info.place_names, info.languages_count, info.alternate_names
            ));
            emit("    License      CC-BY-4.0");
            emit("    Attribution  Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0.");
        }
    }
    Ok(())
}

// --- search ----------------------------------------------------------------

pub fn search(
    project: &Project,
    query_text: &str,
    mut options: SearchOptions,
    out_format: Format,
) -> Outcome {
    let packages = project.installed();
    if packages.is_empty() {
        eprintln!("terrakit: no packages installed.");
        eprintln!("       Run 'terrakit add TR' first.");
        return Err(Exit::NotInstalled);
    }

    let started = Instant::now();
    let mut all = Vec::new();
    let limit = options.effective_limit();

    for package in &packages {
        let conn = open_database(&package.database)?;
        for mut result in query::search(&conn, query_text, &options)? {
            // The package's own country is authoritative when a single-package
            // query was scoped, and otherwise comes from the row.
            result.country = options
                .country
                .clone()
                .unwrap_or_else(|| result.country.clone());
            all.push(result);
        }
        // One package's rows must not crowd out another's.
        options.limit = limit;
    }

    // Same total order as within a package, so the CLI and the SDKs agree even
    // when several packages are installed.
    all.sort_by(|a, b| {
        a.match_kind
            .cmp(&b.match_kind)
            .then_with(|| b.search_rank.cmp(&a.search_rank))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.matched_on.cmp(&b.matched_on))
    });
    all.truncate(limit);

    let elapsed = started.elapsed();

    match out_format {
        Format::Json => {
            let rows: Vec<String> = all
                .iter()
                .map(|r| {
                    format!(
                        "{{\"id\":{},\"name\":{},\"type\":{},\"level\":{},\"country\":{},\
                         \"population\":{},\"parent\":{},\"timezone\":{},\
                         \"latitude\":{},\"longitude\":{},\"matchedOn\":{},\
                         \"match\":{},\"score\":{:.2}}}",
                        jstr(&r.id),
                        jstr(&r.name),
                        jstr(&r.place_type),
                        r.level,
                        jstr(&r.country),
                        r.population
                            .map(|p| p.to_string())
                            .unwrap_or("null".into()),
                        jstr_opt(&r.parent_name),
                        jstr_opt(&r.timezone),
                        r.latitude.map(|v| v.to_string()).unwrap_or("null".into()),
                        r.longitude.map(|v| v.to_string()).unwrap_or("null".into()),
                        jstr(&r.matched_on),
                        jstr(r.match_kind.label()),
                        r.score(),
                    )
                })
                .collect();
            emit(&format!(
                "{{\"query\":{},\"normalized\":{},\"count\":{},\"tookMs\":{},\"results\":[{}]}}",
                jstr(query_text),
                jstr(&normalize(query_text)),
                rows.len(),
                elapsed.as_millis(),
                rows.join(",")
            ));
        }
        Format::Human => {
            if all.is_empty() {
                emit(&format!("No results for {query_text:?}."));
                return Err(Exit::NotFound);
            }
            for r in &all {
                let population = r
                    .population
                    .map(|p| format!("{:>10}", format_thousands(p)))
                    .unwrap_or_else(|| " ".repeat(10));
                let location = match (&r.parent_name, r.parent_name.as_ref().map(|_| ())) {
                    (Some(parent), _) => format!("{parent}, {}", r.country),
                    _ => r.country.clone(),
                };
                emit(&format!(
                    "  {:<10} {:<24} {:<28} {}",
                    r.place_type.to_uppercase(),
                    truncate(&r.name, 24),
                    truncate(&location, 28),
                    population
                ));
            }
            emit("");
            emit(&format!(
                "{} result(s) in {} ms",
                all.len(),
                elapsed.as_millis()
            ));
        }
    }
    Ok(())
}

// --- postal ----------------------------------------------------------------

pub fn postal(project: &Project, country: &str, code: &str, out_format: Format) -> Outcome {
    let packages = project.packages_for(country);
    let Some(package) = packages.last() else {
        eprintln!("terrakit: {country} is not installed.");
        return Err(Exit::NotInstalled);
    };
    let conn = open_database(&package.database)?;
    let hit = query::postal_code(&conn, country, code)?;

    match out_format {
        Format::Json => {
            let areas: Vec<String> = hit
                .areas
                .iter()
                .map(|(name, _)| jstr(name))
                .collect();
            emit(&format!(
                "{{\"code\":{},\"country\":{},\"place\":{},\"latitude\":{},\"longitude\":{},\
                 \"areas\":[{}],\"areaCount\":{}}}",
                jstr(&hit.code),
                jstr(&country.to_uppercase()),
                jstr_opt(&hit.place_name),
                hit.latitude.map(|v| v.to_string()).unwrap_or("null".into()),
                hit.longitude.map(|v| v.to_string()).unwrap_or("null".into()),
                areas.join(","),
                hit.areas.len()
            ));
        }
        Format::Human => {
            emit(&format!("{} ({})", hit.code, country.to_uppercase()));
            if let Some(place) = &hit.place_name {
                emit(&format!("    Primary  {place}"));
            }
            // A code can cover several places; 6 in Türkiye span more than one
            // district. Printing only the primary would hide that.
            emit(&format!("    Areas    {}", hit.areas.len()));
            for (name, _) in &hit.areas {
                emit(&format!("      - {name}"));
            }
        }
    }
    Ok(())
}

// --- doctor ----------------------------------------------------------------

pub fn doctor(project: &Project, out_format: Format) -> Outcome {
    // Diagnostics only. A command a user runs when something is wrong must not
    // make it worse, so nothing here writes.
    let packages = project.installed();
    let mut checks: Vec<(String, String, bool)> = Vec::new();

    checks.push(("CLI".into(), format!("{CLI_VERSION}"), true));
    checks.push((
        "Platform".into(),
        format!("{} {}, {}", std::env::consts::OS, std::env::consts::ARCH, CLI_VERSION),
        true,
    ));

    let mut capabilities: Vec<(String, bool)> = Vec::new();
    if let Some(package) = packages.first() {
        match open_database(&package.database) {
            Ok(conn) => capabilities = query::sqlite_capabilities(&conn),
            Err(_) => {
                checks.push((
                    "Database".into(),
                    format!("cannot open {}", package.database.display()),
                    false,
                ));
            }
        }
    }
    for (name, ok) in &capabilities {
        checks.push((format!("SQLite {name}"), String::new(), *ok));
    }

    checks.push((
        "Project".into(),
        project.root.display().to_string(),
        project.manifest_path.is_file(),
    ));
    checks.push((
        "Packages".into(),
        format!("{} installed", packages.len()),
        !packages.is_empty(),
    ));

    // Integrity of every package, not just the first: one tampered package among
    // several is exactly the case a user needs to be told about. Each result is a
    // row in `checks`, and `all_ok` below already aggregates them.
    for package in &packages {
        let (detail, ok) = match verify::verify_package(&package.directory) {
            Ok(result) => (result.summary().to_string(), result.ok),
            Err(reason) => (reason, false),
        };
        checks.push((format!("Integrity {}", package.country), detail, ok));
    }

    if let Some(package) = packages.first() {
        if let Ok(conn) = open_database(&package.database) {
            let counts = query::counts(&conn);
            // The search index coverage rule from DATA_MODEL.md section 9: every
            // name of 3+ characters must be reachable.
            let searchable: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM (SELECT DISTINCT name_norm, place_id FROM (
                        SELECT name_norm, id AS place_id FROM places WHERE LENGTH(name_norm) >= 3
                        UNION ALL SELECT name_norm, place_id FROM place_names WHERE LENGTH(name_norm) >= 3
                        UNION ALL SELECT name_norm, place_id FROM place_alternate_names WHERE LENGTH(name_norm) >= 3))",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            checks.push((
                "Places".into(),
                format!("{}", counts.places),
                counts.places > 0,
            ));
            checks.push((
                "Names".into(),
                format!(
                    "{} localized, {} alternates",
                    counts.place_names, counts.alternate_names
                ),
                searchable == counts.search_fts,
            ));
            checks.push((
                "Provenance".into(),
                format!("{} source(s)", counts.sources),
                counts.sources > 0,
            ));
        }
    }

    let all_ok = checks.iter().all(|(_, _, ok)| *ok);

    match out_format {
        Format::Json => {
            let rows: Vec<String> = checks
                .iter()
                .map(|(name, detail, ok)| {
                    format!(
                        "{{\"check\":{},\"detail\":{},\"ok\":{}}}",
                        jstr(name),
                        jstr(detail),
                        ok
                    )
                })
                .collect();
            emit(&format!(
                "{{\"ok\":{},\"version\":{},\"checks\":[{}]}}",
                all_ok,
                jstr(CLI_VERSION),
                rows.join(",")
            ));
        }
        Format::Human => {
            emit(&format!("terrakit {CLI_VERSION}"));
            for (name, detail, ok) in &checks {
                let mark = if *ok { "✓" } else { "✗" };
                if detail.is_empty() {
                    emit(&format!("  {mark:<3}{name}"));
                } else {
                    emit(&format!("  {mark:<3}{name:<18}{detail}"));
                }
            }
            emit("");
            emit(if all_ok {
                "All checks passed."
            } else {
                "Some checks failed. See SECURITY.md for what each one protects."
            });
        }
    }

    if !all_ok {
        return Err(Exit::Failure);
    }
    Ok(())
}

// --- add --------------------------------------------------------------------

/// Whether stderr is a terminal.
///
/// Wrapped so the reason lives in one place rather than at each use site: this exists because
/// in-place progress overwrites its line, which is noise anywhere else.
fn is_terminal() -> bool {
    // SAFETY: `isatty` takes a file descriptor, imposes no preconditions, and STDERR_FILENO is
    // the constant 2.
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

/// The resolved settings a command runs under.
///
/// Loaded once in `main` and passed in, so precedence is decided in one place rather than by
/// which function happens to read which source. `CLI_SPEC.md` §3: flag, then environment, then
/// config file, then the compiled-in default.
#[derive(Debug, Clone)]
pub struct Context {
    pub registry: String,
    pub cache_dir: PathBuf,
    pub config: Config,
    pub offline: bool,
}

impl Context {
    pub fn load(
        registry_flag: Option<&str>,
        cache_flag: Option<&Path>,
        offline: bool,
    ) -> Result<Context, String> {
        let path = signature::config_path();
        let config = Config::load(&path).unwrap_or_else(|e| {
            // A malformed config must not be silently ignored: it usually means a release key
            // the user believes is trusted is not, and every install would then be refused with
            // "unknown key", pointing at the registry instead of at the config.
            eprintln!("terrakit: {e}");
            eprintln!("       Fix it, or set TERRAKIT_CONFIG to a different file.");
            std::process::exit(i32::from(Exit::Usage.code()));
        });

        // DEFAULT_REGISTRY is a constant rather than a network lookup, so a fresh install works
        // without a bootstrapping step that could itself be tampered with.
        let registry = registry_flag
            .map(str::to_string)
            .or_else(|| std::env::var("TERRAKIT_REGISTRY").ok())
            .or_else(|| config.registry.clone())
            .unwrap_or_else(|| DEFAULT_REGISTRY.to_string());

        let cache_dir = cache_flag
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("TERRAKIT_CACHE").map(PathBuf::from))
            .or_else(|| config.cache_dir.clone())
            .unwrap_or_else(default_cache_dir);

        Ok(Context {
            registry,
            cache_dir,
            config,
            offline,
        })
    }

    pub fn keyring(&self) -> std::collections::BTreeMap<String, signature::TrustedKey> {
        self.config.keyring()
    }
}

pub const DEFAULT_REGISTRY: &str = "https://registry.terrakit.dev";

pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("terrakit");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".cache").join("terrakit")
}

/// `terrakit add <CC>...`
///
/// The order below is the security contract, not a preference:
///
/// 1. resolve the version from the registry index
/// 2. fetch the manifest and verify its **signature**
/// 3. check compatibility (`schemaVersion`, `minReaderVersion`)
/// 4. check free space
/// 5. download the artifact, verifying the compressed digest
/// 6. decompress into a staging directory, verifying the extracted digest
/// 7. `rename` into place — the install becomes visible atomically
/// 8. record the resolution in `terrakit.json`
///
/// Signing is verified before any bytes of the package are fetched, and before any space is
/// consumed. Fetching first and verifying afterwards would mean an attacker-controlled registry
/// can make the client download an arbitrary size before anything is checked — including before
/// the client knows whether it should be talking to that registry at all.
pub fn add(
    project: &Project,
    countries: &[String],
    context: &Context,
    options: AddOptions,
    out_format: Format,
) -> Outcome {
    if countries.is_empty() {
        return usage_error("add needs at least one country code, e.g. 'terrakit add TR'");
    }

    let mut manifest = match project.read_manifest() {
        Ok(manifest) => manifest,
        Err(reason) => {
            eprintln!("terrakit: {reason}");
            return Err(Exit::NotInstalled);
        }
    };

    let mut installed_any = false;
    let mut last_error: Option<Exit> = None;
    let mut records: Vec<AddRecord> = Vec::new();

    for country in countries {
        let code = match registry::country_code(country) {
            Ok(code) => code,
            Err(reason) => {
                eprintln!("terrakit: {reason}");
                last_error = Some(Exit::Usage);
                continue;
            }
        };

        match add_one(project, &code, &mut manifest, context, &options) {
            Ok(record) => {
                installed_any = true;
                records.push(record);
            }
            Err(code_) => last_error = Some(code_),
        }
    }

    // The manifest is written once, after every country. Writing it per country means a failure
    // halfway through leaves a manifest that promises packages the previous run installed but
    // this one did not — or worse, that promises packages that were never installed at all.
    if installed_any {
        if let Err(e) = project.write_manifest(&manifest) {
            return failure(format!("cannot write {}: {e}", project.manifest_path.display()));
        }
    }

    match out_format {
        Format::Json => {
            emit(&format!(
                "{{\"ok\":{},\"registry\":{},\"installed\":[{}]}}",
                installed_any,
                jstr(&context.registry),
                records
                    .iter()
                    .map(AddRecord::to_json)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        Format::Human => {
            for record in &records {
                emit(&format!("  installed  {}", record.to_human()));
                // SPEC section 29 and CLI_SPEC.md 2.2: the attribution prints on every
                // successful install, not only in the documentation. A GeoNames-derived package
                // is CC-BY precisely because someone downstream gives the credit.
                if let Some((license, lines)) = &record.attribution {
                    emit_attribution(license, lines);
                }
            }
        }
    }

    // A partial failure still fails, after everything that could succeed has run:
    // `terrakit add TR XX GE` installs TR and GE, reports the bad code, and exits non-zero.
    //
    // Returning success because *something* worked is how a CI script ends up green with a
    // country silently missing. The installs are already committed and are not rolled back —
    // discarding working packages because a third argument was a typo helps nobody.
    if let Some(code) = last_error {
        return Err(code);
    }
    if !installed_any {
        return Err(Exit::Failure);
    }
    Ok(())
}

/// What one installed package looks like afterwards.
///
/// A struct rather than a pre-rendered JSON string, because the same fact needs two renderings.
/// Building the JSON eagerly meant the human output printed JSON fragments — which teaches users
/// to pipe the tool into `jq` out of habit and then get confused when the human form is not
/// parseable.
#[derive(Debug, Clone)]
pub struct AddRecord {
    pub country: String,
    pub version: String,
    pub level: Option<String>,
    pub size_bytes: u64,
    pub signed_by: String,
    pub status: &'static str,
    /// Licence and attribution, so a `--json` consumer can reproduce it.
    pub attribution: Option<(String, Vec<String>)>,
}

impl AddRecord {
    /// Render as a JSON object.
    ///
    /// Assembled from two parts rather than one format string so the optional licence fields
    /// land INSIDE the object. Appending them after the closing brace — the obvious way to
    /// avoid an optional trailing comma — produces `{"a":1},"license":"x"`, which is not JSON,
    /// and `add --json` stopped parsing.
    fn to_json(&self) -> String {
        let mut out = format!(
            "{{\"country\":{},\"version\":{},\"level\":{},\"sizeBytes\":{},\"signedBy\":{},\"status\":{}",
            jstr(&self.country),
            jstr(&self.version),
            jstr_opt(&self.level),
            self.size_bytes,
            jstr(&self.signed_by),
            jstr(self.status),
        );
        if let Some((license, lines)) = &self.attribution {
            out.push_str(&format!(
                ",\"license\":{},\"attribution\":[{}]",
                jstr(license),
                lines.iter().map(|l| jstr(l)).collect::<Vec<_>>().join(",")
            ));
        }
        out.push('}');
        out
    }

    fn to_human(&self) -> String {
        let level = self.level.as_deref().unwrap_or("-");
        let size = download::human_bytes(self.size_bytes);
        match self.status {
            "already-installed" => {
                format!("{} {} is already installed ({level}, {size})", self.country, self.version)
            }
            _ => format!(
                "{} {} ({level}, {size}, signed by {})",
                self.country, self.version, self.signed_by
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    /// An explicit version, overriding both the pin in `terrakit.json` and the registry default.
    pub version: Option<String>,
    /// Request a level. Refused if the published version does not provide it — Georgia has no
    /// postal data, so `terrakit add GE --level postal` says so rather than installing a
    /// `cities` package under a `postal` promise.
    pub level: Option<String>,
    /// Reinstall even when the version is already present and intact.
    ///
    /// This is the user asking for a repair: `add TR --force` re-downloads a package the user
    /// suspects is damaged.
    pub force: bool,
    /// Accept a matching package that is already on disk and verifies, instead of reinstalling.
    ///
    /// Set by `update`, which has no reason to re-download a version it already holds. Distinct
    /// from `force` because the two want opposite things from the same situation: a user typing
    /// `--force` wants the bytes replaced, while an update moving the selection onto a version
    /// that happens to still be present wants it adopted.
    pub adopt_verified: bool,
    /// Report what an update would do and change nothing.
    pub check_only: bool,
}

fn add_one(
    project: &Project,
    country: &str,
    manifest: &mut package::ProjectManifest,
    context: &Context,
    options: &AddOptions,
) -> Result<AddRecord, Exit> {
    let package = registry::package_name_for(country).map_err(|e| {
        eprintln!("terrakit: {e}");
        Exit::Usage
    })?;

    progress(&format!(
        "resolving {package} from {}",
        context.registry
    ));

    // A fetch failure is a runtime failure, not "not found": reporting 4 makes a script treat
    // an offline refusal and a genuinely absent country the same way, and sends the user
    // looking for a typo rather than for their network.
    let index = registry::fetch_package(&context.registry, &package, context.offline).map_err(|e| {
        eprintln!("terrakit: {e}");
        if context.offline { Exit::Failure } else { Exit::NotFound }
    })?;

    if index.versions.is_empty() {
        eprintln!("terrakit: {package} has no published versions.");
        return Err(Exit::NotFound);
    }

    // Precedence: an explicit --version, then the pin in terrakit.json, then the registry's
    // own latest. An explicit request must beat a pin, or `terrakit add TR@2026.9.0` after a pin
    // would install the pinned version and report success.
    let requested: VersionRange = match &options.version {
        Some(explicit) => VersionRange::parse(explicit).map_err(|e| {
            eprintln!("terrakit: {e}");
            Exit::Usage
        })?,
        None => match manifest.packages.get(country) {
            Some(pinned) => VersionRange::parse(pinned).map_err(|e| {
                eprintln!("terrakit: terrakit.json pins {country} to {pinned:?}: {e}");
                Exit::Usage
            })?,
            None => VersionRange::Any,
        },
    };

    let chosen = registry::select_version(&index.versions, &requested).map_err(|e| {
        eprintln!("terrakit: {e}");
        Exit::NotFound
    })?;

    let published_level = chosen.level.clone().or_else(|| {
        index.latest.as_ref().and_then(|l| l.level.clone())
    });
    if let Some(wanted) = options.level.as_ref() {
        if published_level.as_deref() != Some(wanted.as_str()) {
            eprintln!(
                "terrakit: {package}@{} is published at level {:?}, not {wanted:?}.",
                chosen.version,
                published_level.as_deref().unwrap_or("unknown")
            );
            if published_level.as_deref() == Some("cities") && wanted == "postal" {
                eprintln!(
                    "       GeoNames publishes no postal file for {country}, so the pipeline\n       demoted this package to 'cities'. The manifest declares that, rather\n       than shipping a package that returns no postal codes."
                );
            }
            return Err(Exit::NotFound);
        }
    }

    let version = chosen.version.clone();
    let manifest_url = registry::manifest_for(&context.registry, &package, chosen);
    let download_url = registry::download_for(&context.registry, &package, chosen).ok_or_else(|| {
        eprintln!("terrakit: {package}@{version} has no download URL in the registry.");
        Exit::Verification
    })?;

    // --- 2. manifest and signature ------------------------------------------
    let manifest_bytes = download::fetch_text(&manifest_url, context.offline).map_err(|e| {
        eprintln!("terrakit: {e}");
        Exit::Failure
    })?;

    let signature_url = registry::signature_for(&manifest_url);
    // A missing signature is a verification failure, not a transport failure, and the two must
    // not share an exit code. Reporting "cannot fetch signature.json" for a package that was
    // simply never signed points the user at their network instead of at the package, and
    // returns 1 where a CI policy keys on 3 to mean "do not trust this".
    let signature_text = match download::fetch_text(&signature_url, context.offline) {
        Ok(text) => text,
        Err(reason) => {
            let missing = reason.contains("No such file")
                || reason.contains("cannot read")
                || reason.contains("404");
            if missing {
                eprintln!("terrakit: package verification failed.");
                eprintln!("       {}", signature::SignatureFailure::Missing);
                return Err(Exit::Verification);
            }
            eprintln!("terrakit: {reason}");
            return Err(Exit::Failure);
        }
    };

    let keyring = context.keyring();
    let key_id = signature::verify(&signature_text, manifest_bytes.as_bytes(), &keyring).map_err(
        |failure| {
            eprintln!("terrakit: package verification failed.");
            eprintln!("       {failure}");
            // A refused signature is the one failure that must never be retried and must never
            // be reported as a runtime error: it is exit 3.
            Exit::Verification
        },
    )?;
    progress(&format!("  signature verified (keyId={key_id})"));

    // --- 3. compatibility ----------------------------------------------------
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_bytes).map_err(|e| {
        eprintln!("terrakit: {manifest_url} is not valid JSON: {e}");
        Exit::Verification
    })?;

    check_compatibility(&manifest_value)?;

    // --- 4. space ------------------------------------------------------------
    let database_entry = manifest_value
        .get("files")
        .and_then(|files| files.as_array())
        .and_then(|files| {
            files
                .iter()
                .find(|entry| entry.get("path").and_then(|p| p.as_str()) == Some("data.sqlite"))
        })
        .ok_or_else(|| {
            eprintln!("terrakit: the manifest has no data.sqlite entry.");
            Exit::Verification
        })?;

    let expected_sha256 = database_entry
        .get("sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            eprintln!("terrakit: the manifest records no sha256 for data.sqlite.");
            Exit::Verification
        })?;
    let compressed_sha256 = database_entry
        .get("compressedSha256")
        .and_then(|v| v.as_str());
    let extracted_size = database_entry
        .get("rawSize")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let compressed_size = database_entry
        .get("compressedSize")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let compression = database_entry
        .get("compression")
        .and_then(|v| v.as_str())
        .unwrap_or("zstd");

    let destination = project
        .packages_dir
        .join(country)
        .join(registry::version_to_path_segment(&version));

    if destination.exists() {
        let existing = crate::verify::verify_package(&destination);
        let intact = existing.as_ref().map(|v| v.ok).unwrap_or(false);

        if intact && (options.adopt_verified || !options.force) {
            // Report what is actually on disk rather than a zero placeholder: `--json` saying
            // 0 B for an installed 9.9 MB package is worse than no size field, because a script
            // that sums it gets a plausible wrong answer.
            progress(&format!(
                "  {country}@{version} is already installed; nothing to do (--force to reinstall)"
            ));
            let integrity = existing
                .as_ref()
                .map(|v| v.summary().to_string())
                .unwrap_or_else(|_| "unverifiable".to_string());
            record_resolution(manifest, country, &version, published_level.as_deref());
            return Ok(AddRecord {
                country: country.to_string(),
                version,
                level: published_level.clone(),
                size_bytes: install::dir_size(&destination),
                signed_by: integrity,
                status: "already-installed",
                attribution: None,
            });
        }

        if !options.force {
            // Present, bytes do not match the manifest, and no instruction to replace them.
            // Reporting this as "already installed" would be the worst outcome available: the
            // user is told the package is fine while it does not verify.
            let detail = existing
                .as_ref()
                .map(|v| v.summary().to_string())
                .unwrap_or_else(|e| format!("unverifiable: {e}"));
            eprintln!(
                "terrakit: {country}@{version} is installed but {detail}."
            );
            eprintln!(
                "       Nothing has been changed. Re-run with --force to replace it."
            );
            return Err(Exit::Verification);
        }

        // --force: the package is being replaced on purpose. Remove it before staging so the
        // commit's existence check passes, and so a failure leaves nothing rather than a
        // half-replaced directory.
        progress(&format!("  replacing the installed {country}@{version}"));
        if let Err(e) = std::fs::remove_dir_all(&destination) {
            eprintln!("terrakit: cannot replace {}: {e}", destination.display());
            return Err(Exit::Failure);
        }
    }

    let plan = install::InstallPlan {
        country: country.to_string(),
        version: version.clone(),
        destination: destination.clone(),
        extracted_size,
        compressed_size,
    };
    install::check_space(&plan).map_err(|e| {
        eprintln!("terrakit: {e}");
        Exit::NoSpace
    })?;

    // --- 5. download ---------------------------------------------------------
    let artifact = artifact_name(compression);
    let expected_download_digest = compressed_sha256.unwrap_or(expected_sha256);
    let cache_target = download::cache_path(
        &context.cache_dir,
        expected_download_digest,
        &format!(".{}", artifact.rsplit('.').next().unwrap_or("bin")),
    );

    // A cache entry is used only after it verifies. Keying by digest makes the filename a
    // claim rather than a fact, and an interrupted write, a truncated file or a poisoned cache
    // would otherwise install without a download at all.
    let cached = cache_target.is_file()
        && download::verify_file(&cache_target, expected_download_digest).is_ok();

    if cached {
        progress(&format!(
            "  cached   {}",
            download::human_bytes(compressed_size)
        ));
    } else {
        progress(&format!(
            "  downloading {}",
            download::human_bytes(compressed_size)
        ));
    }
    // In-place progress overwrites its line with \r, which is only meaningful on a terminal.
    // Piped or captured, it collects every intermediate percentage on one line.
    let interactive = is_terminal();
    let mut last_percent = -1i32;
    let outcome = if cached {
        // A `--offline` install works whenever the artifact is already here, which is the
        // whole point of a cache and the only way an air-gapped reinstall is possible.
        if context.offline {
            progress("  using the cached artifact (--offline)");
        }
        Ok(download::FetchOutcome {
            bytes: compressed_size,
            resumed_from: 0,
            attempts: 0,
        })
    } else {
        download::download(
            &download_url,
            &cache_target,
            Some(expected_download_digest),
            context.offline,
            &mut |received, total| {
                // Progress on stderr: stdout carries data (`CLI_SPEC.md` section 1).
                if interactive && total > 0 {
                    let percent = (received as f64 / total as f64 * 100.0) as i32;
                    if percent >= last_percent + 10 {
                        last_percent = percent;
                        eprint!("\r    {percent:>3}%  {}", download::human_bytes(received));
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                    }
                }
            },
        )
    };
    if interactive && !cached {
        eprintln!();
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("terrakit: {e}");
            return Err(download::exit_for(&e));
        }
    };
    if cached {
        progress("  from cache");
    } else {
        progress(&format!("  {}", fetch_summary(&outcome)));
    }

    // --- 6. stage ------------------------------------------------------------
    let staged = install::stage(
        &cache_target,
        &destination,
        &registry::version_to_path_segment(&version),
        expected_sha256,
    )
    .map_err(|e| {
        eprintln!("terrakit: {e}");
        download::exit_for(&e)
    })?;

    // The manifest and signature travel with the package, so a later `terrakit validate` can
    // prove which key signed the copy on disk rather than only that its bytes are unchanged.
    // A failure here discards the staged directory, so nothing half-written survives. The
    // package is never committed without its manifest and signature, because `terra validate`
    // could then not say which key signed what is on disk.
    for (name, bytes) in [
        ("manifest.json", manifest_bytes.as_bytes()),
        ("signature.json", signature_text.as_bytes()),
    ] {
        if let Err(e) = std::fs::write(staged.directory.join(name), bytes) {
            install::discard(&staged);
            eprintln!("terrakit: cannot write {name} into the package: {e}");
            return Err(Exit::Failure);
        }
    }

    // --- 7. commit -----------------------------------------------------------
    if destination.exists() {
        install::discard(&staged);
        eprintln!(
            "terrakit: {} already exists. Refusing to overwrite it.",
            destination.display()
        );
        return Err(Exit::Failure);
    }
    let committed = match install::commit(staged, &destination) {
        Ok(committed) => committed,
        Err(e) => {
            eprintln!("terrakit: {e}");
            return Err(Exit::Failure);
        }
    };

    record_resolution(manifest, country, &version, published_level.as_deref());
    progress(&format!(
        "  installed {} ({})",
        download::human_bytes(committed.size_on_disk),
        destination.display()
    ));

    let (license, attribution_lines) = attribution_of(&manifest_value);

    Ok(AddRecord {
        country: country.to_string(),
        version,
        level: published_level.clone(),
        size_bytes: committed.size_on_disk,
        signed_by: key_id.clone(),
        status: "installed",
        attribution: Some((license, attribution_lines)),
    })
}

/// Format a fetch outcome for the progress line.
fn fetch_summary(outcome: &download::FetchOutcome) -> String {
    format!("downloaded {}", outcome.summary())
}

/// The licence and attribution strings a successful install obliges the user to reproduce.
fn attribution_of(manifest: &serde_json::Value) -> (String, Vec<String>) {
    let license = manifest
        .get("license")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let lines: Vec<String> = manifest
        .get("attribution")
        .and_then(|v| v.as_array())
        .map(|entries| entries.iter().filter_map(|e| e.as_str()).map(String::from).collect())
        .unwrap_or_default();

    // The fallback is not a courtesy: `terra info` prints it too, and a package whose
    // attribution exists only in the documentation is a licence violation waiting to happen.
    if lines.is_empty() {
        if let Some(sources) = manifest.get("sources").and_then(|v| v.as_array()) {
            return (
                license,
                sources
                    .iter()
                    .take(3)
                    .map(|source| {
                        source
                            .get("attribution")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no attribution recorded)")
                            .to_string()
                    })
                    .collect(),
            );
        }
    }
    (license, lines)
}

/// Print the attribution block, for human output only.
///
/// `--json` puts nothing but JSON on stdout (`CLI_SPEC.md` section 1), and this block arrived
/// there after that contract was written. The strings themselves travel inside the JSON record,
/// so nothing is lost: a script that has to reproduce an attribution reads it from the machine
/// output, and a person reads it on their terminal.
fn emit_attribution(license: &str, lines: &[String]) {
    emit(&format!("    License      {license}"));
    for line in lines {
        emit(&format!("    Attribution  {line}"));
    }
}

/// On-disk name of a compressed artifact.
///
/// Must match `COMPRESSION_EXTENSIONS` in `release.py`. Duplicated rather than imported because
/// one is Python and one is Rust and the alternative is shipping the pipeline as a library; the
/// pairing is covered by the end-to-end test in `scripts/test.sh`, which installs from a real
/// built registry and would fail with a 404 if the two drifted.
fn artifact_name(compression: &str) -> String {
    match compression {
        "zstd" => "data.sqlite.zst".to_string(),
        "gzip" => "data.sqlite.gz".to_string(),
        _ => "data.sqlite".to_string(),
    }
}

/// A semantic version as a comparable integer: `0.1.0` becomes `1000`.
///
/// Used only for `minReaderVersion`. Comparing version components numerically matters here for
/// the same reason it does in `registry.rs`: a lexicographic comparison puts `0.10.0` below
/// `0.9.0`, which would let a CLI claim it can read a package it cannot.
fn normalize_version_number(version: &str) -> u64 {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    major * 1_000_000 + minor * 1_000 + patch
}

fn record_resolution(
    manifest: &mut package::ProjectManifest,
    country: &str,
    version: &str,
    level: Option<&str>,
) {
    manifest
        .packages
        .entry(country.to_string())
        .or_insert_with(|| format!("^{version}"));
    // `manifest.level` is the project's *default* for future installs and is deliberately not
    // updated here. Georgia's package is demoted to "cities" because GeoNames publishes no
    // postal file for it, and writing that into the project default left a project that also
    // installed TR reporting a level no package has. The per-package level is in `resolution`.
    let _ = level;
    let resolution = manifest
        .resolution
        .get_or_insert_with(Default::default);
    resolution.insert(
        country.to_string(),
        package::ResolvedPackage {
            version: version.to_string(),
            level: level.map(str::to_string),
            integrity: Some("verified".to_string()),
        },
    );
}

/// Refuse a package this build cannot read, before spending bandwidth on it.
fn check_compatibility(manifest: &serde_json::Value) -> Result<(), Exit> {
    const SUPPORTED_SCHEMA: u64 = 1;

    let schema = manifest
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(SUPPORTED_SCHEMA);
    if schema > SUPPORTED_SCHEMA {
        eprintln!(
            "terrakit: this package uses schema version {schema}, and this CLI supports \
             {SUPPORTED_SCHEMA}."
        );
        eprintln!(
            "       Upgrade with 'cargo install terrakit-cli' or your package manager. \
             Installing it anyway would produce a database this build cannot read correctly."
        );
        return Err(Exit::Verification);
    }

    let min_reader = manifest
        .get("minReaderVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let this_version = normalize_version_number(CLI_VERSION);
    if min_reader > this_version {
        eprintln!(
            "terrakit: this package requires reader version {min_reader}, and this CLI is \
             {CLI_VERSION}."
        );
        return Err(Exit::Verification);
    }
    Ok(())
}

// --- remove ------------------------------------------------------------------

/// `terrakit remove <CC>...`
///
/// Removes the country's package directory and drops it from `terrakit.json`. The shared
/// download cache is left alone unless `--prune` is given, because the cache is shared across
/// projects: one project's `remove` deleting an artifact another project is about to install
/// turns a local operation into a network round trip somewhere else.
///
/// `cli remove` deliberately does *not* touch anything else. In particular it leaves other
/// versions of the same country alone unless asked, because a package directory per version is
/// what makes a rollback possible and nothing here knows whether the user wants that.
pub fn remove(
    project: &Project,
    countries: &[String],
    cache_dir: &Path,
    prune_cache: bool,
    out_format: Format,
) -> Outcome {
    if countries.is_empty() {
        return usage_error("remove needs at least one country code, e.g. 'terrakit remove TR'");
    }

    let mut manifest = match project.read_manifest() {
        Ok(manifest) => manifest,
        Err(reason) => {
            eprintln!("terrakit: {reason}");
            return Err(Exit::NotInstalled);
        }
    };

    let mut removed: Vec<RemoveRecord> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut freed: u64 = 0;

    for country in countries {
        let code = match registry::country_code(country) {
            Ok(code) => code,
            Err(reason) => {
                eprintln!("terrakit: {reason}");
                return Err(Exit::Usage);
            }
        };

        let installed = project.packages_for(&code);
        if installed.is_empty() {
            missing.push(code.clone());
            continue;
        }

        // Reported before deletion, because afterwards there is nothing left to report. A
        // package that was already corrupt is worth knowing about: the user asked to remove it
        // for a reason, and "it was already broken" is usually the explanation.
        let mut records = Vec::new();
        for package in &installed {
            let (integrity, size) = match crate::verify::verify_package(&package.directory) {
                Ok(result) => (result.summary().to_string(), package.size_on_disk()),
                Err(reason) => (format!("unreadable: {reason}"), package.size_on_disk()),
            };
            freed += size;
            records.push(RemoveRecord {
                country: code.clone(),
                version: package.version.clone(),
                level: package.level.clone(),
                size_bytes: size,
                integrity,
            });
        }

        let directory = project.packages_dir.join(&code);
        if let Err(e) = std::fs::remove_dir_all(&directory) {
            eprintln!("terrakit: cannot remove {}: {e}", directory.display());
            return Err(Exit::Failure);
        }

        manifest.packages.remove(&code);
        if let Some(resolution) = manifest.resolution.as_mut() {
            resolution.remove(&code);
        }
        removed.extend(records);
    }

    // One write, after every country. A per-country write would leave a manifest claiming a
    // package is installed after its directory has been deleted, which `list` and `search`
    // both trust.
    if !removed.is_empty() {
        if let Err(e) = project.write_manifest(&manifest) {
            return failure(format!("cannot write {}: {e}", project.manifest_path.display()));
        }
    }

    if prune_cache {
        prune_cache_dir(project, cache_dir);
    }

    match out_format {
        Format::Json => emit(&format!(
            "{{\"ok\":{},\"removed\":[{}],\"notInstalled\":[{}],\"freedBytes\":{},\"cachePruned\":{}}}",
            !removed.is_empty(),
            removed
                .iter()
                .map(RemoveRecord::to_json)
                .collect::<Vec<_>>()
                .join(","),
            missing
                .iter()
                .map(|c| jstr(c))
                .collect::<Vec<_>>()
                .join(","),
            freed,
            prune_cache,
        )),
        Format::Human => {
            for record in &removed {
                emit(&format!(
                    "  removed  {}",
                    record.to_human()
                ));
            }
            if !missing.is_empty() {
                for code in &missing {
                    progress(&format!("  {code} is not installed; nothing to remove"));
                }
            }
            if freed > 0 {
                emit(&format!("\n  freed {}", download::human_bytes(freed)));
            }
        }
    }

    // Removing nothing that was asked for is not a failure: `terrakit remove TR DE` where DE
    // was never installed removed TR and reported DE. Failing would make the useful part of the
    // command look broken. But a request that removed nothing at all exits 6, so a script can
    // tell "did what you asked" from "there was nothing there".
    if removed.is_empty() {
        return Err(Exit::NotInstalled);
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RemoveRecord {
    pub country: String,
    pub version: String,
    pub level: String,
    pub size_bytes: u64,
    pub integrity: String,
}

impl RemoveRecord {
    fn to_json(&self) -> String {
        format!(
            "{{\"country\":{},\"version\":{},\"level\":{},\"sizeBytes\":{},\"integrityBeforeRemoval\":{}}}",
            jstr(&self.country),
            jstr(&self.version),
            jstr(&self.level),
            self.size_bytes,
            jstr(&self.integrity),
        )
    }

    fn to_human(&self) -> String {
        format!(
            "{} {} ({}, {}, was {})",
            self.country,
            self.version,
            self.level,
            download::human_bytes(self.size_bytes),
            self.integrity
        )
    }
}

/// Drop download artifacts no installed package refers to.
///
/// Keyed by digest, so this cannot delete an artifact some other project is about to install:
/// an entry is removed only when no installed manifest names its digest.
fn prune_cache_dir(project: &Project, cache: &Path) {
    let artifacts = cache.join("artifacts");
    let Ok(shards) = std::fs::read_dir(&artifacts) else {
        return;
    };

    let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for package in project.installed() {
        if let Ok(manifest) = crate::verify::read_manifest_bytes(&package.manifest_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&manifest) {
                if let Some(files) = value.get("files").and_then(|f| f.as_array()) {
                    for entry in files {
                        if let Some(digest) = entry
                            .get("compressedSha256")
                            .and_then(|v| v.as_str())
                        {
                            referenced.insert(digest.to_string());
                        }
                        if let Some(digest) = entry.get("sha256").and_then(|v| v.as_str()) {
                            referenced.insert(digest.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut freed = 0u64;
    let mut removed = 0usize;
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(digest) = name.split('.').next() else {
                continue;
            };
            if referenced.contains(digest) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                freed += meta.len();
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        progress(&format!(
            "  pruned {removed} cache entr{} ({}), unreferenced by any installed package",
            if removed == 1 { "y" } else { "ies" },
            download::human_bytes(freed)
        ));
    }
}

// --- update ------------------------------------------------------------------

/// `terrakit update [CC]...`
///
/// Installs the newest published version that satisfies the project's pin, then removes the
/// version it replaced.
///
/// Removing the old version is not tidiness. `search` opens every installed package, so two
/// versions of one country return the same places twice with identical IDs — and the caller has
/// no way to tell a duplicate from a real second result. One active version per country is what
/// keeps the result set honest.
pub fn update(
    project: &Project,
    countries: &[String],
    context: &Context,
    options: &AddOptions,
    out_format: Format,
) -> Outcome {
    let mut manifest = match project.read_manifest() {
        Ok(manifest) => manifest,
        Err(reason) => {
            eprintln!("terrakit: {reason}");
            return Err(Exit::NotInstalled);
        }
    };

    let installed = project.installed();
    if installed.is_empty() {
        eprintln!("terrakit: no packages installed.");
        return Err(Exit::NotInstalled);
    }

    // No arguments means every installed country, which is what `terrakit update` alone means.
    let targets: Vec<String> = if countries.is_empty() {
        let mut codes: Vec<String> = installed.iter().map(|p| p.country.clone()).collect();
        codes.sort();
        codes.dedup();
        codes
    } else {
        countries.to_vec()
    };

    let mut findings: Vec<UpdateFinding> = Vec::new();
    let mut changed_any = false;
    let mut failed = false;

    for country in &targets {
        let code = match registry::country_code(country) {
            Ok(code) => code,
            Err(reason) => {
                eprintln!("terrakit: {reason}");
                return Err(Exit::Usage);
            }
        };

        let current: Vec<&InstalledPackage> =
            installed.iter().filter(|p| p.country == code).collect();
        if current.is_empty() {
            findings.push(UpdateFinding {
                country: code.clone(),
                current: None,
                latest: None,
                action: UpdateAction::NotInstalled,
                note: Some("not installed; use 'terrakit add'".into()),
            });
            failed = true;
            continue;
        }

        let package = match registry::package_name_for(&code) {
            Ok(name) => name,
            Err(reason) => {
                eprintln!("terrakit: {reason}");
                return Err(Exit::Usage);
            }
        };

        let index = match registry::fetch_package(&context.registry, &package, context.offline) {
            Ok(index) => index,
            Err(reason) => {
                eprintln!("terrakit: {code}: {reason}");
                findings.push(UpdateFinding {
                    country: code.clone(),
                    current: current.first().map(|p| p.version.clone()),
                    latest: None,
                    action: UpdateAction::Unknown,
                    note: Some(reason.lines().next().unwrap_or("unavailable").to_string()),
                });
                failed = true;
                continue;
            }
        };

        // The range an update may move within. An explicit --to wins; otherwise the project's
        // own pin applies, so `update` cannot escape a constraint the user wrote down. With no
        // pin, any published version qualifies, which is what makes the first `add` write a pin
        // and every later `update` respect it.
        let requested: VersionRange = match &options.version {
            Some(explicit) => VersionRange::parse(explicit).map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Usage
            })?,
            None => match manifest.packages.get(&code) {
                Some(pinned) => VersionRange::parse(pinned).map_err(|e| {
                    eprintln!("terrakit: terrakit.json pins {code} to {pinned:?}: {e}");
                    Exit::Usage
                })?,
                None => VersionRange::Any,
            },
        };

        let latest = index.latest.as_ref().map(|l| l.version.clone());
        let target = match registry::select_version(&index.versions, &requested) {
            Ok(entry) => entry.version.clone(),
            Err(reason) => {
                eprintln!("terrakit: {code}: {reason}");
                findings.push(UpdateFinding {
                    country: code.clone(),
                    current: current.first().map(|p| p.version.clone()),
                    latest,
                    action: UpdateAction::Unknown,
                    note: Some("no version satisfies the pin".into()),
                });
                failed = true;
                continue;
            }
        };

        // "Current" is the version the project SELECTED, not the newest on disk.
        //
        // Those differ after a rollback or an explicit `--version` install: the directory may
        // hold 2026.11.0 while terrakit.json still selects 2026.10.1. Using the newest on disk
        // made update believe it was already current, then try to install a version whose
        // directory existed, and refuse. The selection is what `search` resolves against and
        // what a user means by "the version I have".
        let current_version = manifest
            .resolution
            .as_ref()
            .and_then(|r| r.get(&code))
            .map(|r| r.version.clone())
            .or_else(|| {
                current
                    .iter()
                    .map(|p| p.version.clone())
                    .max_by(|a, b| compare_versions(a, b))
            })
            .unwrap_or_default();

        if target == current_version {
            findings.push(UpdateFinding {
                country: code.clone(),
                current: Some(current_version.clone()),
                latest,
                action: UpdateAction::UpToDate,
                note: None,
            });
            continue;
        }

        if options.check_only {
            findings.push(UpdateFinding {
                country: code.clone(),
                current: Some(current_version.clone()),
                latest: Some(target.clone()),
                action: UpdateAction::Available,
                note: None,
            });
            continue;
        }

        progress(&format!(
            "updating {code}: {current_version} -> {target}"
        ));

        // add_one re-verifies, re-checks space and installs atomically. Reused rather than
        // reimplemented because a second install path would be a second set of bugs, and this
        // is the same operation with a different starting version.
        let install_options = AddOptions {
            version: Some(target.clone()),
            level: options.level.clone(),
            // Not `force`: the target may already be on disk and verified, and re-downloading
            // it would be pure waste. `adopt_verified` takes it as it finds it.
            force: false,
            adopt_verified: true,
            check_only: false,
        };
        match add_one(project, &code, &mut manifest, context, &install_options) {
            Ok(_) => {
                changed_any = true;
                // Every version on disk except the target, not merely the one that was
                // selected. A rollback can leave two directories behind, and leaving either
                // means `search` returns the same places twice with identical IDs.
                let superseded: Vec<String> = current
                    .iter()
                    .map(|p| p.version.clone())
                    .filter(|v| *v != target)
                    .collect();
                let mut freed = 0;
                for version in &superseded {
                    let old = project
                        .packages_dir
                        .join(&code)
                        .join(registry::version_to_path_segment(version));
                    freed += install::dir_size(&old);
                    if let Err(e) = std::fs::remove_dir_all(&old) {
                        // A leftover old version is not fatal: the new one is installed and
                        // correct. It is, however, a duplicate in every search result, so it
                        // is reported rather than swallowed.
                        eprintln!(
                            "terrakit: warning: cannot remove the superseded version {}: {e}",
                            old.display()
                        );
                        findings.push(UpdateFinding {
                            country: code.clone(),
                            current: Some(current_version.clone()),
                            latest: Some(target.clone()),
                            action: UpdateAction::Updated,
                            note: Some(format!(
                                "could not remove the old version at {}; search results will                                  include it twice",
                                old.display()
                            )),
                        });
                        continue;
                    }
                }
                findings.push(UpdateFinding {
                    country: code.clone(),
                    current: Some(current_version.clone()),
                    latest: Some(target.clone()),
                    action: UpdateAction::Updated,
                    note: if freed > 0 {
                        Some(format!("freed {}", download::human_bytes(freed)))
                    } else {
                        None
                    },
                });
            }
            Err(code_) => {
                eprintln!("terrakit: {code}: update failed.");
                failed = true;
                findings.push(UpdateFinding {
                    country: code.clone(),
                    current: Some(current_version.clone()),
                    latest: Some(target.clone()),
                    action: UpdateAction::Failed,
                    note: Some(format!("exit {}", code_.code())),
                });
            }
        }
    }

    if changed_any {
        if let Err(e) = project.write_manifest(&manifest) {
            return failure(format!("cannot write {}: {e}", project.manifest_path.display()));
        }
    }

    let all_ok = !failed;
    match out_format {
        Format::Json => emit(&format!(
            "{{\"ok\":{},\"checkOnly\":{},\"changed\":{},\"packages\":[{}]}}",
            all_ok,
            options.check_only,
            changed_any,
            findings
                .iter()
                .map(UpdateFinding::to_json)
                .collect::<Vec<_>>()
                .join(",")
        )),
        Format::Human => {
            for finding in &findings {
                emit(&format!("  {}", finding.to_human()));
            }
        }
    }

    if failed {
        return Err(Exit::Failure);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    UpToDate,
    Available,
    Updated,
    NotInstalled,
    Unknown,
    Failed,
}

#[derive(Debug, Clone)]
pub struct UpdateFinding {
    pub country: String,
    pub current: Option<String>,
    pub latest: Option<String>,
    pub action: UpdateAction,
    pub note: Option<String>,
}

impl UpdateFinding {
    fn label(&self) -> &'static str {
        match self.action {
            UpdateAction::UpToDate => "up to date",
            UpdateAction::Available => "update available",
            UpdateAction::Updated => "updated",
            UpdateAction::NotInstalled => "not installed",
            UpdateAction::Unknown => "unknown",
            UpdateAction::Failed => "failed",
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"country\":{},\"current\":{},\"latest\":{},\"action\":{},\"note\":{}}}",
            jstr(&self.country),
            jstr_opt(&self.current),
            jstr_opt(&self.latest),
            jstr(self.label()),
            jstr_opt(&self.note),
        )
    }

    fn to_human(&self) -> String {
        let arrow = match (&self.current, &self.latest) {
            (Some(current), Some(latest)) => format!("{current} -> {latest}"),
            (Some(current), None) => format!("{current} -> ?"),
            (None, Some(latest)) => format!("- -> {latest}"),
            (None, None) => "-".to_string(),
        };
        let note = self
            .note
            .as_ref()
            .map(|n| format!("  ({n})"))
            .unwrap_or_default();
        format!("{:<12} {:<32} {}{note}", self.country, arrow, self.label())
    }
}

/// Numeric version comparison, for "which of these installed versions is current".
///
/// Numeric rather than string ordering: `2026.10.10` is newer than `2026.10.9`, and a
/// lexicographic comparison gets that backwards and would then remove the newer package during
/// an update.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    registry::Version::parse(a)
        .cmp(&registry::Version::parse(b))
        .then_with(|| a.cmp(b))
}

// --- cache ------------------------------------------------------------------

/// `terrakit cache <list|clean|path|verify>`
///
/// Thin output over [`crate::cache`], which owns the semantics and the tests. The split is
/// because the rules here are the interesting part and they are not about formatting.
pub fn cache(
    project: &Project,
    cache_dir: &Path,
    subcommand: &str,
    all: bool,
    out_format: Format,
) -> Outcome {
    match subcommand {
        "path" => {
            // Machine-readable on purpose: this is what a build script interpolates, and a
            // human-readable line with decoration would be pasted into it verbatim.
            match out_format {
                Format::Json => emit(&format!("{{\"path\":{}}}", jstr(&cache_dir.display().to_string()))),
                Format::Human => emit(&cache_dir.display().to_string()),
            }
            Ok(())
        }

        "list" => {
            let entries = crate::cache::list(cache_dir, Some(project));
            let total: u64 = entries.iter().map(|e| e.size).sum();
            let referenced = entries.iter().filter(|e| e.referenced).count();

            match out_format {
                Format::Json => {
                    let rows: Vec<String> = entries
                        .iter()
                        .map(|entry| {
                            format!(
                                "{{\"digest\":{},\"file\":{},\"sizeBytes\":{},\"ageSeconds\":{},\"referenced\":{}}}",
                                jstr(&entry.digest),
                                jstr(&entry.path.display().to_string()),
                                entry.size,
                                entry.age.as_secs(),
                                entry.referenced
                            )
                        })
                        .collect();
                    emit(&format!(
                        "{{\"path\":{},\"count\":{},\"totalBytes\":{},\"referenced\":{},\"entries\":[{}]}}",
                        jstr(&cache_dir.display().to_string()),
                        entries.len(),
                        total,
                        referenced,
                        rows.join(",")
                    ));
                }
                Format::Human => {
                    if entries.is_empty() {
                        emit("Cache is empty.");
                        return Ok(());
                    }
                    emit(&format!(
                        "  {:<64}  {:>9}  {:>5}  {}",
                        "DIGEST", "SIZE", "AGE", "STATE"
                    ));
                    for entry in &entries {
                        let state = if !entry.looks_like_digest {
                            "BAD NAME".to_string()
                        } else if entry.referenced {
                            "in use".to_string()
                        } else {
                            "unused".to_string()
                        };
                        emit(&format!(
                            "  {}  {:>9}  {:>5}  {}",
                            &entry.digest[..entry.digest.len().min(16)],
                            download::human_bytes(entry.size),
                            crate::cache::human_age(entry.age),
                            state
                        ));
                    }
                    emit("");
                    emit(&format!(
                        "{} entr{}, {} total, {referenced} in use",
                        entries.len(),
                        if entries.len() == 1 { "y" } else { "ies" },
                        download::human_bytes(total)
                    ));
                }
            }
            Ok(())
        }

        "clean" => {
            let report = crate::cache::clean(cache_dir, Some(project), all).map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Failure
            })?;

            match out_format {
                Format::Json => emit(&format!(
                    "{{\"ok\":true,\"all\":{},\"removed\":{},\"freedBytes\":{},\"kept\":{}}}",
                    all,
                    report.removed.len(),
                    report.freed,
                    report.kept.len()
                )),
                Format::Human => {
                    if report.removed.is_empty() {
                        emit(
                            &(if all {
                                "Cache is already empty.".to_string()
                            } else {
                                // Says why, because "nothing to remove" with entries present
                                // reads as a bug and sends the user looking for one.
                                format!(
                                    "Nothing to remove. {} of {} cached entr{} in use; the rest\n\
                                     are too recent to be certainly unused by another project.",
                                    report.kept.len(),
                                    report.kept.len(),
                                    if report.kept.len() == 1 { "y is" } else { "ies are" }
                                )
                            }),
                        );
                    } else {
                        emit(&format!(
                            "removed {} cache entr{} ({})",
                            report.removed.len(),
                            if report.removed.len() == 1 { "y" } else { "ies" },
                            download::human_bytes(report.freed)
                        ));
                    }
                    for (digest, reason) in &report.kept {
                        progress(&format!(
                            "  kept {}… ({reason})",
                            &digest[..digest.len().min(12)]
                        ));
                    }
                }
            }
            Ok(())
        }

        "verify" => {
            let (checked, bytes, failures) = crate::cache::verify(cache_dir).map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Failure
            })?;

            match out_format {
                Format::Json => {
                    let rows: Vec<String> = failures
                        .iter()
                        .map(|failure| {
                            format!(
                                "{{\"digest\":{},\"reason\":{},\"expected\":{},\"actual\":{}}}",
                                jstr(&failure.digest),
                                jstr(&failure.reason),
                                jstr(&failure.expected),
                                jstr(&failure.actual)
                            )
                        })
                        .collect();
                    emit(&format!(
                        "{{\"ok\":{},\"checked\":{},\"bytes\":{},\"failures\":[{}]}}",
                        failures.is_empty(),
                        checked,
                        bytes,
                        rows.join(",")
                    ));
                }
                Format::Human => {
                    if failures.is_empty() {
                        emit(&format!(
                            "{} cache entr{} verified, {}",
                            checked,
                            if checked == 1 { "y" } else { "ies" },
                            download::human_bytes(bytes)
                        ));
                    } else {
                        eprintln!("terrakit: {} cache entries failed verification:", failures.len());
                        for failure in &failures {
                            eprintln!("  {}  {}", &failure.digest[..16.min(failure.digest.len())], failure.reason);
                        }
                        eprintln!("  Removed. A cache entry that fails its own digest has no value.");
                    }
                }
            }
            Err(crate::cache::exit_for_verify(failures.len()))
        }

        other => usage_error(format!(
            "unknown cache subcommand {other:?}. Use list, clean, path or verify."
        )),
    }
}

// --- bundle -----------------------------------------------------------------

/// `terrakit bundle --platform <ios|android|all>`
///
/// Progress goes to stderr and the report to stdout, so `--json` emits one document.
pub fn bundle(project: &Project, plan: &crate::bundle::BundlePlan, out_format: Format) -> Outcome {
    let packages = crate::bundle::plan(project).map_err(|e| {
        eprintln!("terrakit: {e}");
        Exit::NotInstalled
    })?;

    // Checked here rather than inside `emit`, because the two failures are different. A project
    // with nothing installed is `6` ("a required package is not installed"); `emit` returning
    // empty is a programming error, and reporting it as `6` would send a user looking for a
    // package they never asked for.
    if packages.is_empty() {
        eprintln!("terrakit: no packages are installed; nothing to bundle.");
        eprintln!("       Run 'terrakit add <CC>' first.");
        return Err(Exit::NotInstalled);
    }

    let report = crate::bundle::emit(plan, &packages).map_err(|e| {
        eprintln!("terrakit: {e}");
        crate::bundle::exit_for(&e)
    })?;

    match out_format {
        Format::Json => emit(&report.to_json()),
        Format::Human => {
            for line in report.to_lines() {
                emit(&line);
            }
            progress(
                "Add bundle/ios/terrakit as an Xcode folder reference, or bundle/android/assets/terrakit \
                 to the Gradle assets.",
            );
        }
    }
    Ok(())
}

// --- helpers ---------------------------------------------------------------

fn format_thousands(value: i64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Truncate on a character boundary.
///
/// `String::truncate` panics on a non-char-boundary index, which is exactly what
/// would happen cutting a name containing Turkish or Japanese text at a byte
/// offset. That is a panic on a user-supplied string in a search result, so it
/// gets an explicit helper.
fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let kept: String = input.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_handles_control_characters() {
        assert_eq!(jstr("plain"), "\"plain\"");
        assert_eq!(jstr("a\"b"), "\"a\\\"b\"");
        assert_eq!(jstr("a\\b"), "\"a\\\\b\"");
        assert_eq!(jstr("a\nb"), "\"a\\nb\"");
        assert_eq!(jstr("a\tb"), "\"a\\tb\"");
        assert_eq!(jstr("a\u{1}b"), "\"a\\u0001b\"");
        // Non-ASCII passes through as UTF-8 rather than being escaped, which keeps
        // the output readable for the names this project is about.
        assert_eq!(jstr("İstanbul"), "\"İstanbul\"");
        assert_eq!(jstr("イスタンブール"), "\"イスタンブール\"");
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate("Antalya", 10), "Antalya");
        assert_eq!(truncate("Antalya", 4), "Ant…");
        // A byte-based truncation here would panic on a mid-character index.
        assert_eq!(truncate("İstanbul", 4), "İst…");
        assert_eq!(truncate("İstanbul", 8), "İstanbul");
        assert_eq!(truncate("東京都", 2), "東…");
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        use std::cmp::Ordering;

        // `update` removes the version it superseded, so picking the wrong "current" deletes
        // the wrong directory. Lexicographic ordering makes 2026.10.9 look newer than
        // 2026.10.10.
        assert_eq!(compare_versions("2026.10.10", "2026.10.9"), Ordering::Greater);
        assert_eq!(compare_versions("2026.11.0", "2026.10.9"), Ordering::Greater);
        assert_eq!(compare_versions("2026.9.1", "2026.10.1"), Ordering::Less);
        // A build suffix does not change the release line, so the full string breaks the tie.
        assert_eq!(compare_versions("2026.10.1", "2026.10.1+a6c3"), Ordering::Less);
        assert_eq!(compare_versions("2026.10.1+a", "2026.10.1+a"), Ordering::Equal);
    }

    #[test]
    fn update_findings_render_both_directions() {
        let forward = UpdateFinding {
            country: "GE".into(),
            current: Some("2026.10.1+e83e".into()),
            latest: Some("2026.11.0+f00d".into()),
            action: UpdateAction::Available,
            note: None,
        };
        let line = forward.to_human();
        assert!(line.contains("2026.10.1+e83e -> 2026.11.0+f00d"), "{line}");
        assert!(line.contains("update available"), "{line}");
        assert!(forward.to_json().contains("\"action\":\"update available\""));

        // A registry that could not be read must not claim "up to date": that is the answer
        // that silently hides a broken --registry from a user who expected fresh information.
        let unknown = UpdateFinding {
            country: "GE".into(),
            current: Some("2026.10.1+e83e".into()),
            latest: None,
            action: UpdateAction::Unknown,
            note: Some("registry unreachable".into()),
        };
        assert!(unknown.to_human().contains("-> ?"), "{}", unknown.to_human());
        assert!(unknown.to_human().contains("registry unreachable"));
    }

    #[test]
    fn a_remove_record_reports_the_integrity_it_had_before_removal() {
        // The package is gone afterwards, so this is the only place the user learns it was
        // already broken -- which is usually why they wanted it gone.
        let record = RemoveRecord {
            country: "TR".into(),
            version: "2026.10.1+9920".into(),
            level: "postal".into(),
            size_bytes: 10_354_688,
            integrity: "modified".into(),
        };
        let human = record.to_human();
        assert!(human.contains("was modified"), "{human}");
        assert!(human.contains("9.9 MB"), "{human}");
        assert!(record.to_json().contains("integrityBeforeRemoval"));
    }

    #[test]
    fn reader_versions_compare_numerically_not_as_text() {
        // Lexicographic ordering puts "0.10.0" below "0.9.0", so a CLI on 0.9 would claim it
        // can read a package requiring 0.10 and fail at the first query instead of at install.
        assert!(normalize_version_number("0.10.0") > normalize_version_number("0.9.0"));
        assert!(normalize_version_number("1.0.0") > normalize_version_number("0.99.99"));
        assert_eq!(normalize_version_number("0.1.0"), 1_000);
        // Build metadata and pre-release suffixes are not part of the ordering.
        assert_eq!(normalize_version_number("1.2.3+a6c3"), 1_002_003);
        assert_eq!(normalize_version_number("1.2.3-rc1"), 1_002_003);
    }

    #[test]
    fn artifact_names_match_the_pipeline() {
        assert_eq!(artifact_name("zstd"), "data.sqlite.zst");
        assert_eq!(artifact_name("gzip"), "data.sqlite.gz");
        // An unknown or absent method means the uncompressed artifact, which is what the
        // manifest's null compression means.
        assert_eq!(artifact_name(""), "data.sqlite");
    }

    #[test]
    fn thousands_separator_groups_from_the_right() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1 000");
        assert_eq!(format_thousands(15_701_602), "15 701 602");
        assert_eq!(format_thousands(1_335_002), "1 335 002");
    }
}
