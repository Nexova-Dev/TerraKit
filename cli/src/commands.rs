//! Command implementations.
//!
//! One module per command group, dispatched from `main.rs`. Human output goes to
//! stderr; only machine-readable output goes to stdout. That split is the reason
//! `terrakit add TR --json` is usable in a script while `terrakit add TR` shows a
//! progress bar in the same terminal — see `CLI_SPEC.md` §1.

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use crate::exit::{Exit, Outcome, failure, usage_error};
use crate::normalize::normalize;
use crate::package::{self, Project, human_bytes, human_coord, open_database};
use crate::verify;
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
            "{{\"name\":\"terrakit\",\"version\":{},\n \"rustc\":{},\"target\":{}}}",
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
    fn thousands_separator_groups_from_the_right() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1 000");
        assert_eq!(format_thousands(15_701_602), "15 701 602");
        assert_eq!(format_thousands(1_335_002), "1 335 002");
    }
}
