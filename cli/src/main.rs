//! `terrakit` — the TerraKit command-line tool.
//!
//! Argument parsing is hand-written. `CLI_SPEC.md` fixes the command surface, the
//! exit codes and the stdout/stderr split, and those are the contract; a parser
//! crate would obscure them behind its own conventions for a CLI with twelve
//! commands and no configuration language.

use std::path::PathBuf;

// The binary depends on its own library rather than declaring the modules again.
// Two crate roots compiling the same modules is duplicated compilation and a
// standing invitation for them to diverge; the conformance tests import the
// library, so the binary must use the same code they do.
use terrakit_cli::{commands, exit, package, query};

use commands::Format;
use exit::{Exit, Outcome, usage_error};

const USAGE: &str = "\
terrakit — universal geographic data package manager

USAGE
    terrakit <command> [arguments] [flags]

COMMANDS
    init                    Create terrakit.json in the current directory
    add <CC>...             Install or upgrade country packages
    remove <CC>...          Uninstall country packages
    update [CC]...          Check for and apply newer dataset versions
    list                    Installed packages
    search <query>          Search across installed packages
    info <CC>               Country metadata and entity counts
    validate [CC]           Run the SPEC section 30 checks
    doctor                  Environment and installation diagnostics
    cache <subcommand>      Manage the shared download cache
    version                 Version of the CLI
    install <source>        Install from a GitHub source
    bundle --platform <p>   Emit platform resource manifests

GLOBAL FLAGS
    --json                  Machine-readable output on stdout
    --quiet                 Suppress progress on stderr
    --verbose               Add timings and resolved paths
    --no-color              Disable colour (NO_COLOR is honoured)
    --registry <url>        Alternate registry, for mirrors and testing
    --cache-dir <path>      Override the shared cache location
    --platform <name>       bundle: ios, android or all
    --out <dir>             bundle: destination for the resource manifest tree
    --offline               Fail rather than reach the network
    --help                  Show this message

EXIT CODES
    0  success
    1  runtime failure
    2  usage error
    3  verification failure — checksum, signature or schema mismatch
    4  nothing found
    5  insufficient disk space
    6  a required package is not installed

EXAMPLES
    terrakit init
    terrakit add TR
    terrakit search Antalya
    terrakit search Constantinople --country TR
    terrakit info TR --json
";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => Exit::Ok.into(),
        Err(exit) => exit.into(),
    }
}

struct Flags {
    json: bool,
    country: Option<String>,
    city: Option<String>,
    types: Vec<String>,
    limit: Option<usize>,
    level: Option<String>,
    platform: Option<String>,
    strict: bool,
    force: bool,
    cache_dir: Option<PathBuf>,
    /// The package version requested by `--version`, as opposed to the CLI's own version.
    package_version: Option<String>,
    registry: Option<String>,
    offline: bool,
    check_only: bool,
    /// `remove --prune`: also drop unreferenced cache entries.
    prune: bool,
    keep_cache: bool,
    /// `bundle --out <dir>`: where the resource manifest tree is written.
    bundle_dir: Option<PathBuf>,
    /// `cache clean --all`: remove everything, not only prunable entries.
    clean_all: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Flags {
            json: false,
            country: None,
            city: None,
            types: Vec::new(),
            limit: None,
            level: None,
            platform: None,
            strict: false,
            force: false,
            cache_dir: None,
            package_version: None,
            registry: None,
            offline: false,
            check_only: false,
            prune: false,
            keep_cache: false,
            bundle_dir: None,
            clean_all: false,
        }
    }
}

fn run(args: &[String]) -> Outcome {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }

    // A leading `--flag` means no command was given: `terrakit --version` and
    // `terrakit --json` are queries about the tool, not invocations of something. Taking
    // `--version` as a command name would report "unknown command" for a flag that is in the
    // usage text.
    let has_command = !args[0].starts_with('-');
    let command = if has_command { args[0].clone() } else { String::new() };
    let rest = if has_command { &args[1..] } else { &args[..] };

    // `--json` is read in a pre-pass because `--version` can be answered mid-parse, and
    // `terrakit --version --json` must produce JSON regardless of which flag came first.
    // Reading it only in the main loop would make the output depend on argument order.
    let wants_json = args.iter().any(|a| a == "--json");
    let used_to_flag = rest.iter().any(|a| a == "--to");

    // Flags are separated from positionals in one pass so `--json` works before
    // or after the positional arguments, which is what people actually type.
    let mut positionals: Vec<String> = Vec::new();
    let mut flags = Flags::default();
    let mut index = 0;

    while index < rest.len() {
        let arg = &rest[index];
        let mut take_value = |name: &str| -> Result<String, Exit> {
            index += 1;
            rest.get(index)
                .cloned()
                .ok_or_else(|| exit::Exit::Usage)
                .map_err(|_| {
                    eprintln!("terrakit: {name} requires a value");
                    Exit::Usage
                })
        };

        match arg.as_str() {
            "--json" => flags.json = true,
            "--quiet" | "--no-color" => {}
            "--offline" => flags.offline = true,
            "--verbose" => {}
            // `update --check` reports without changing anything, and `--check-only` is the
            // unambiguous spelling. Both are honoured rather than one, because a script that
            // passes `--check` and gets an install would be a very bad afternoon.
            "--check" | "--check-only" | "--dry-run" => flags.check_only = true,
            "--prune" => flags.prune = true,
            // `cache clean --all` is documented (CLI_SPEC.md 2.10). Distinct from --force, which
            // means "replace an installed package".
            "--all" => flags.clean_all = true,
            "--keep-cache" => flags.keep_cache = true,
            "--strict" => flags.strict = true,
            "--force" => flags.force = true,
            "--registry" => flags.registry = Some(take_value("--registry")?),
            // Level shorthands, per CLI_SPEC.md's `terrakit add TR --postal`. Spelled as
            // flags because that is what the examples show and what people type; they are
            // the same setting as --level, not a separate code path.
            "--basic" => flags.level = Some("basic".into()),
            "--administrative" => flags.level = Some("administrative".into()),
            "--cities" => flags.level = Some("cities".into()),
            "--postal" => flags.level = Some("postal".into()),
            "--full" => flags.level = Some("full".into()),
            "--country" | "-c" => flags.country = Some(take_value("--country")?),
            "--city" => flags.city = Some(take_value("--city")?),
            "--type" => {
                let value = take_value("--type")?;
                flags.types.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                );
            }
            "--limit" => {
                let value = take_value("--limit")?;
                match value.parse::<usize>() {
                    Ok(n) => flags.limit = Some(n),
                    Err(_) => {
                        return usage_error(format!("--limit expects a number, got {value:?}"));
                    }
                }
            }
            "--level" | "-l" => flags.level = Some(take_value("--level")?),
            "--platform" => flags.platform = Some(take_value("--platform")?),
            "--cache-dir" => flags.cache_dir = Some(PathBuf::from(take_value("--cache-dir")?)),
            "--out" => flags.bundle_dir = Some(PathBuf::from(take_value("--out")?)),
            // `--version` is overloaded, and the overload is the documented one:
            // `terrakit add TR --version 2026.10.1` asks for a package version, while
            // `terrakit --version` reports the CLI's own. Disambiguated by whether a
            // command was given, which is available here because the command is parsed
            // before the flags.
            // `--to` is `update`'s documented spelling (CLI_SPEC.md 2.4) and `--version` is the
            // global one; both set the same value, so a user who reaches for either gets it.
            "--to" => flags.package_version = Some(take_value("--to")?),
            "--version" | "-V" => {
                if command.is_empty() {
                    let early = if wants_json { Format::Json } else { Format::Human };
                    return commands::version(early);
                }
                flags.package_version = Some(take_value("--version")?);
            }
            other if other.starts_with("--") => {
                return usage_error(format!("unknown flag {other}"));
            }
            other => positionals.push(other.to_string()),
        }
        index += 1;
    }

    let out_format = if flags.json { Format::Json } else { Format::Human };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    match command.as_str() {
        "version" => commands::version(out_format),

        "init" => commands::init(&cwd, flags.level.as_deref(), flags.force, out_format),

        "list" | "ls" => {
            let project = package::require_project(&cwd)?;
            commands::list(&project, out_format)
        }

        "info" => {
            let Some(country) = positionals.first() else {
                return usage_error("info needs a country code, e.g. 'terrakit info TR'");
            };
            let project = package::require_project(&cwd)?;
            commands::info(&project, country, out_format)
        }

        "search" => {
            let Some(query) = positionals.first() else {
                return usage_error("search needs a query, e.g. 'terrakit search Antalya'");
            };
            let project = package::require_project(&cwd)?;
            let mut options = query::SearchOptions {
                country: flags.country.clone(),
                city: flags.city.clone(),
                types: flags.types.clone(),
                limit: flags.limit.unwrap_or(0),
            };
            if let Some(country) = flags.country.as_ref() {
                // A country filter is also a package filter: searching only TR
                // should not open and scan every installed package.
                let upper = country.to_uppercase();
                let packages = project.packages_for(&upper);
                if packages.is_empty() {
                    eprintln!("terrakit: {upper} is not installed.");
                    return Err(Exit::NotInstalled);
                }
                options.country = Some(upper);
            }
            commands::search(&project, query, options, out_format)
        }

        "postal" => {
            let Some(country) = positionals.first() else {
                return usage_error("postal needs a country code");
            };
            let Some(code) = positionals.get(1) else {
                return usage_error("postal needs a code, e.g. 'terrakit postal TR 07160'");
            };
            let project = package::require_project(&cwd)?;
            commands::postal(&project, country, code, out_format)
        }

        "doctor" => match package::Project::discover(&cwd) {
            Some(project) => commands::doctor(&project, out_format),
            None => {
                // `doctor` must work outside a project: it is the command a user
                // runs when something is wrong, including when the project is not
                // what they think it is.
                commands::doctor(&package::Project::at(cwd), out_format)
            }
        },

        "validate" => {
            let project = package::require_project(&cwd)?;
            commands_validate(&project, positionals.first().map(|s| s.as_str()), flags.strict, out_format)
        }

        // Specified in CLI_SPEC.md but not built yet. Reported as "not
        // implemented" rather than "unknown command", because the difference
        // matters: the first says the design is settled and the work is pending,
        // the second says the user made a typo.
        //
        // Exit::Failure rather than a new code: CLI_SPEC.md §1 fixes the code
        // table, and adding an undeclared one would break the contract a script
        // depends on. `1` correctly means "did not do what you asked".
        // Shared by `add` and `update`: both resolve settings the same way, and a difference
        // would mean `update` read a different registry than the `add` that installed the
        // package.
        "add" | "update" => {
            let project = package::require_project(&cwd)?;
            let context = commands::Context::load(
                flags.registry.as_deref(),
                flags.cache_dir.as_deref(),
                flags.offline,
            )
            .map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Usage
            })?;
            let options = commands::AddOptions {
                version: flags.package_version.clone(),
                level: flags.level.clone(),
                force: flags.force,
                adopt_verified: false,
                check_only: flags.check_only,
            };
            if command == "add" {
                if used_to_flag {
                    return usage_error(
                        "--to is `update`'s flag. For a fresh install use --version, \
                         as in 'terrakit add TR --version 2026.10.1'.",
                    );
                }
                commands::add(&project, &positionals, &context, options, out_format)
            } else {
                commands::update(&project, &positionals, &context, &options, out_format)
            }
        }

        "remove" | "rm" | "uninstall" => {
            let project = package::require_project(&cwd)?;
            // The cache directory is resolved through the same Context `add` uses. Reading it
            // from the environment instead meant `remove --cache-dir X --prune` silently
            // pruned a different directory than the one the installs had written to.
            let context = commands::Context::load(
                flags.registry.as_deref(),
                flags.cache_dir.as_deref(),
                flags.offline,
            )
            .map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Usage
            })?;
            // The shared cache survives removal by default, which is what `--keep-cache` says
            // and what makes the cache worth having. `--prune` opts into dropping entries no
            // installed package refers to. `--keep-cache` is accepted explicitly and wins over
            // `--prune`, because the two together should mean the safe one.
            let prune = flags.prune && !flags.keep_cache;
            commands::remove(&project, &positionals, &context.cache_dir, prune, out_format)
        }

        "cache" => {
            let project = package::require_project(&cwd)?;
            let context = terrakit_cli::commands::Context::load(
                flags.registry.as_deref(),
                flags.cache_dir.as_deref(),
                flags.offline,
            )
            .map_err(|e| {
                eprintln!("terrakit: {e}");
                Exit::Usage
            })?;
            let Some(subcommand) = positionals.first().map(String::as_str) else {
                return usage_error(
                    "cache needs a subcommand: list, clean, path or verify",
                );
            };
            commands::cache(
                &project,
                &context.cache_dir,
                subcommand,
                flags.clean_all,
                out_format,
            )
        }

        "bundle" => {
            let project = package::require_project(&cwd)?;
            let Some(platform) = flags.platform.as_deref() else {
                return usage_error(
                    "bundle needs --platform ios, --platform android or --platform all",
                );
            };
            let platforms = if platform.eq_ignore_ascii_case("all") {
                vec![terrakit_cli::bundle::Platform::Ios, terrakit_cli::bundle::Platform::Android]
            } else {
                match terrakit_cli::bundle::Platform::parse(platform) {
                    Some(one) => vec![one],
                    None => {
                        return usage_error(format!(
                            "{platform:?} is not a platform. Use ios, android or all."
                        ))
                    }
                }
            };
            let plan = terrakit_cli::bundle::BundlePlan {
                out_dir: flags
                    .bundle_dir
                    .clone()
                    .unwrap_or_else(|| cwd.join("bundle")),
                platforms,
                verify: true,
            };
            commands::bundle(&project, &plan, out_format)
        }

        // Specified in CLI_SPEC.md but not built: it needs GitHub tarball access, which is a
        // separate transport with its own trust story. Reported as "not implemented" rather than
        // "unknown command", because the difference matters — the first says the design is
        // settled and the work is pending, the second says the user made a typo.
        //
        // Exit::Failure rather than a new code: CLI_SPEC.md section 1 fixes the code table,
        // and adding an undeclared one would break the contract a script depends on.
        other if COMMANDS_NOT_YET_BUILT.contains(&other) => {
            eprintln!("terrakit: '{other}' is specified in CLI_SPEC.md but not implemented yet.");
            eprintln!("       It needs the package registry, which is not hosted yet.");
            eprintln!("       See CONTRIBUTING.md section 3 for the build order.");
            Err(Exit::Failure)
        }

        "" => usage_error("no command given. Run 'terrakit --help' for the command list."),
        other => usage_error(format!(
            "unknown command {other:?}. Run 'terrakit --help' for the command list."
        )),
    }
}

const COMMANDS_NOT_YET_BUILT: &[&str] = &["install"];

fn commands_validate(
    project: &package::Project,
    country: Option<&str>,
    strict: bool,
    out_format: Format,
) -> Outcome {
    let packages = match country {
        Some(code) => project.packages_for(code),
        None => project.installed(),
    };
    if packages.is_empty() {
        eprintln!("terrakit: nothing to validate.");
        return Err(Exit::NotInstalled);
    }

    let mut all_ok = true;
    let mut warning_total: i64 = 0;
    let mut rows: Vec<String> = Vec::new();

    for package in &packages {
        // Integrity first. `CLI_SPEC.md` §1 reserves exit code 3 for
        // verification failure, and a checksum mismatch is the one thing that must
        // produce it — a `validate` that opens a tampered database, finds its
        // structure intact and reports success is worse than no `validate` at all.
        //
        // Checking this before opening the database also means a corrupted file is
        // reported as corruption rather than as a SQLite error.
        let integrity = match terrakit_cli::verify::verify_package(&package.directory) {
            Ok(result) => result,
            Err(reason) => {
                all_ok = false;
                if out_format == Format::Json {
                    rows.push(format!(
                        "{{\"country\":{},\"version\":{},\"ok\":false,\"integrity\":{},\"reason\":{}}}",
                        commands::jstr(&package.country),
                        commands::jstr(&package.version),
                        commands::jstr("unverifiable"),
                        commands::jstr(&reason)
                    ));
                } else {
                    emit_line(&format!("✗ {} {}", package.country, reason));
                }
                continue;
            }
        };

        let conn = match package::open_database(&package.database) {
            Ok(conn) => conn,
            Err(_) => {
                // The reason was already printed by `open_database`. Repeating it
                // here would duplicate it in the human output and in the JSON.
                all_ok = false;
                rows.push(format!(
                    "{{\"country\":{},\"version\":{},\"ok\":false,\"reason\":{}}}",
                    commands::jstr(&package.country),
                    commands::jstr(&package.version),
                    commands::jstr("cannot open the database")
                ));
                continue;
            }
        };
        let counts = query::counts(&conn);
        let check = validate_package(&conn);
        // A checksum mismatch is an error alongside the structural ones, so it
        // flows through the same reporting path and the same exit code.
        let mut errors = check.errors;
        if !integrity.ok {
            let modified: Vec<&str> = integrity
                .checks
                .iter()
                .filter(|c| {
                    c.status != terrakit_cli::verify::FileStatus::Verified
                })
                .map(|c| match c.status {
                    terrakit_cli::verify::FileStatus::Modified => "modified",
                    terrakit_cli::verify::FileStatus::Missing => "missing",
                    _ => "unexpected",
                })
                .collect();
            errors.push(format!(
                "integrity: {} ({})",
                integrity.summary(),
                errors_label(&modified)
            ));
        }
        let ok = errors.is_empty();
        all_ok &= ok;
        warning_total += check.warnings.iter().sum::<i64>();

        if out_format == Format::Human {
            emit_line(&format!(
                "{} {} ({}) — {}",
                if ok { "✓" } else { "✗" },
                package.country,
                package.version,
                integrity.summary()
            ));
        }

        match out_format {
            Format::Human => {
                emit_line(&format!(
                    "    places {}, closure {}, postal {} codes / {} areas",
                    counts.places, counts.closure, counts.postal_codes, counts.postal_areas
                ));
                emit_line(&format!(
                    "    names {} localized, {} alternates, {} phonetic codes",
                    counts.place_names, counts.alternate_names, counts.phonetic
                ));
                emit_line(&format!(
                    "    search index {} rows, spatial index {} entries",
                    counts.search_fts, counts.rtree
                ));
                for finding in &errors {
                    emit_line(&format!("  ✗ {finding}"));
                }
                let warnings = check.warnings.len();
                if warnings > 0 {
                    emit_line(&format!("  ! {warnings} warning(s)"));
                }
                emit_line(&format!("    errors: {}, warnings: {}", errors.len(), warnings));
            }
            Format::Json => {
                rows.push(format!(
                    "{{\"country\":{},\"version\":{},\"level\":{},\"ok\":{},\
                     \"counts\":{{\"places\":{},\"closure\":{},\"postalCodes\":{},\"postalAreas\":{},\
                     \"placeNames\":{},\"alternateNames\":{},\"searchFts\":{},\"phonetic\":{},\
                     \"rtree\":{},\"sources\":{}}},\"integrity\":{},\"errors\":{},\"warnings\":{}}}",
                    commands::jstr(&package.country),
                    commands::jstr(&package.version),
                    commands::jstr(&package.level),
                    ok,
                    counts.places, counts.closure, counts.postal_codes, counts.postal_areas,
                    counts.place_names, counts.alternate_names, counts.search_fts,
                    counts.phonetic, counts.rtree, counts.sources,
                    commands::jstr(integrity.summary()),
                    errors.len(),
                    check.warnings.len(),
                ));
            }
        }
    }

    if out_format == Format::Json {
        emit_json(&rows, all_ok);
    }

    // `--strict` promotes warnings to failures, which is what CI wants and what a
    // human run does not. Warnings are never hidden either way: they are real
    // information about upstream data quality, and `SPEC.md` §30 requires them
    // surfaced with a count.
    if strict && warning_total > 0 {
        all_ok = false;
    }

    if all_ok {
        Ok(())
    } else {
        Err(Exit::Verification)
    }
}

/// The rules from `DATA_MODEL.md` §13 that the CLI can check without reimplementing
/// the pipeline.
///
/// Deliberately a subset. The pipeline's validator is authoritative and blocks
/// publication; this one answers "is the copy on my disk still intact", which is a
/// different question and is what a user asks when a package behaves oddly.
fn validate_package(conn: &rusqlite::Connection) -> ValidationSummary {
    let mut summary = ValidationSummary::default();

    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };

    // Closure completeness: every place must be reachable from itself at depth 0.
    let places = one("SELECT COUNT(*) FROM places");
    let closure_self = one("SELECT COUNT(*) FROM place_closure WHERE depth = 0");
    if closure_self != places {
        summary.errors.push(format!(
            "closure table covers {closure_self} of {places} places at depth 0"
        ));
    }

    // Referential integrity. Cheap, and it catches a truncated package.
    let dangling_parents = one(
        "SELECT COUNT(*) FROM places p WHERE p.parent_id IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM places q WHERE q.id = p.parent_id)",
    );
    if dangling_parents > 0 {
        summary.errors
            .push(format!("{dangling_parents} places reference a missing parent"));
    }

    let dangling_closure = one(
        "SELECT COUNT(*) FROM place_closure c \
         WHERE NOT EXISTS (SELECT 1 FROM places p WHERE p.id = c.descendant_id)",
    );
    if dangling_closure > 0 {
        summary
            .errors
            .push(format!("{dangling_closure} closure rows reference a missing place"));
    }

    // Search index coverage, the rule from DATA_MODEL.md section 9. Counting
    // DISTINCT (name_norm, place_id) because that is what search_fts holds: two
    // places sharing a name each need their own row, while alternates that
    // normalize onto their own place's name collapse to one on purpose.
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
    let indexed = one("SELECT COUNT(*) FROM search_fts");
    if searchable != indexed {
        summary.errors.push(format!(
            "{searchable} searchable names but {indexed} rows in search_fts"
        ));
    }

    // Spatial coverage: anything with coordinates must be in the R*Tree, or
    // `nearest` silently cannot find it.
    let with_coords = one("SELECT COUNT(*) FROM places WHERE latitude IS NOT NULL");
    let rtree = one("SELECT COUNT(*) FROM places_rtree");
    if with_coords != rtree {
        summary.errors.push(format!(
            "{with_coords} places have coordinates but {rtree} are in places_rtree"
        ));
    }

    // Provenance: a package that cannot prove its origin is not shippable
    // (LICENSING.md section 6).
    let sources = one("SELECT COUNT(*) FROM data_sources");
    if sources == 0 {
        summary
            .errors
            .push("no data_sources rows; attribution cannot be established".into());
    }
    let incomplete = one(
        "SELECT COUNT(*) FROM data_sources \
         WHERE TRIM(license) = '' OR TRIM(attribution) = '' OR TRIM(license_url) = ''",
    );
    if incomplete > 0 {
        summary.errors.push(format!(
            "{incomplete} data source rows have an empty license or attribution"
        ));
    }

    // Duplicate sibling names are a warning, not an error: real data has them.
    summary.warnings.push(one(
        "SELECT COUNT(*) FROM (SELECT parent_id, type, name_norm FROM places \
         GROUP BY parent_id, type, name_norm HAVING COUNT(*) > 1)",
    ));

    summary
}

/// Compact label for the integrity failures, so the message says which files and not
/// just how many.
fn errors_label(statuses: &[&str]) -> String {
    match statuses {
        [] => "none".to_string(),
        [one] => (*one).to_string(),
        many => {
            let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
            for status in many {
                *counts.entry(status).or_default() += 1;
            }
            counts
                .into_iter()
                .map(|(status, n)| format!("{n}x {status}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

#[derive(Default)]
struct ValidationSummary {
    errors: Vec<String>,
    warnings: Vec<i64>,
}

/// One line of human-readable validation output.
///
/// stdout, like every other read command's results: stdout is data, stderr is
/// progress. A `--json` run must get nothing but JSON here, which is what
/// `CLI_SPEC.md` §1 requires.
fn emit_line(text: &str) {
    let mut out = std::io::stdout().lock();
    use std::io::Write;
    let _ = writeln!(out, "{text}");
}

fn emit_json(rows: &[String], ok: bool) {
    let mut out = std::io::stdout().lock();
    use std::io::Write;
    let _ = writeln!(out, "{{\"ok\":{ok},\"packages\":[{}]}}", rows.join(","));
}
