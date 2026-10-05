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
use crate::package::{self, Project, human_bytes, human_coord, open_database};
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
                let version = manifest
                    .resolution
                    .as_ref()
                    .and_then(|r| r.get(&p.country))
                    .map(|r| r.version.clone())
                    .unwrap_or_else(|| p.version.clone());
                // Checksum verification, not signature verification: this is what
                // catches a package tampered with locally, which a valid signature
                // does not. See verify.rs and SECURITY.md section 7.
                let integrity = verify::verify_package(&p.directory)
                    .map(|v| v.summary())
                    .unwrap_or("unverifiable");
                emit(&format!(
                    "  {:<4}  {:<12}  {:<14}  {:>9}  {}",
                    p.country,
                    p.level,
                    version,
                    human_bytes(p.size_on_disk()),
                    integrity
                ));
            }
            emit("");
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
}

impl AddRecord {
    fn to_json(&self) -> String {
        format!(
            "{{\"country\":{},\"version\":{},\"level\":{},\"sizeBytes\":{},\"signedBy\":{},\"status\":{}}}",
            jstr(&self.country),
            jstr(&self.version),
            jstr_opt(&self.level),
            self.size_bytes,
            jstr(&self.signed_by),
            jstr(self.status),
        )
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
    /// Reinstall even when the version is already present.
    pub force: bool,
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

    if destination.exists() && !options.force {
        // Report what is actually on disk rather than a zero placeholder: `--json` output that
        // says 0 B for an installed 9.9 MB package is worse than no size field, because a script
        // that sums it gets a plausible wrong answer.
        if let Some(existing) = project
            .installed()
            .into_iter()
            .find(|p| p.country == country && p.version == version)
        {
            progress(&format!(
                "  {country}@{version} is already installed; nothing to do (--force to reinstall)"
            ));
            let integrity = crate::verify::verify_package(&existing.directory)
                .map(|v| v.summary().to_string())
                .unwrap_or_else(|_| "unverifiable".to_string());
            record_resolution(manifest, country, &version, published_level.as_deref());
            return Ok(AddRecord {
                country: country.to_string(),
                version,
                level: published_level.clone(),
                size_bytes: existing.size_on_disk(),
                signed_by: integrity,
                status: "already-installed",
            });
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
    let cache_target = download::cache_path(
        &context.cache_dir,
        compressed_sha256.unwrap_or(expected_sha256),
        &format!(".{}", artifact.rsplit('.').next().unwrap_or("bin")),
    );

    progress(&format!(
        "  downloading {}",
        download::human_bytes(compressed_size)
    ));
    // In-place progress overwrites its line with \r, which is only meaningful on a terminal.
    // Piped or captured, it collects every intermediate percentage on one line.
    let interactive = is_terminal();
    let mut last_percent = -1i32;
    let outcome = download::download(
        &download_url,
        &cache_target,
        compressed_sha256.or(Some(expected_sha256)),
        context.offline,
        &mut |received, total| {
            // Progress on stderr: stdout carries data (`CLI_SPEC.md` §1).
            if interactive && total > 0 {
                let percent = (received as f64 / total as f64 * 100.0) as i32;
                if percent >= last_percent + 10 {
                    last_percent = percent;
                    eprint!("\r    {percent:>3}%  {}", download::human_bytes(received));
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                }
            }
        },
    );
    if interactive {
        eprintln!();
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("terrakit: {e}");
            return Err(download::exit_for(&e));
        }
    };
    progress(&format!("  downloaded {}", outcome.summary()));

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

    Ok(AddRecord {
        country: country.to_string(),
        version,
        level: published_level.clone(),
        size_bytes: committed.size_on_disk,
        signed_by: key_id.clone(),
        status: "installed",
    })
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
