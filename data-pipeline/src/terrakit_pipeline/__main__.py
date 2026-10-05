"""
Pipeline entry point.

    python -m terrakit_pipeline build TR --level postal --out ../datasets
    python -m terrakit_pipeline validate ../datasets/terrakit-data-tr-2026.10.1

Stages, in SPEC section 27 order. Each prints what it did, so a build log is a
usable audit record rather than a status line.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import UTC, datetime
from pathlib import Path

from .alternate_names import detect_script, read_alternate_names
from .download import fetch_all
from .geonames import (
    read_admin1,
    read_admin2,
    read_country_info,
    read_postal_zip,
    read_settlements,
)
from .hierarchy import build_hierarchy
from .names import attach_names, primary_language_of
from .normalize import normalize, postal_normalize
from .package_builder import (
    PackageMetadata,
    build_package,
    default_source_date_epoch,
)
from .phonetic import double_metaphone
from .validator import validate_build, validate_package
from .version import GENERATOR_VERSION, dataset_version_for

LEVELS = ("basic", "administrative", "cities", "postal", "full")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="terrakit-pipeline", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    build_cmd = sub.add_parser("build", help="build one country package")
    build_cmd.add_argument("country", help="ISO 3166-1 alpha-2, e.g. TR")
    build_cmd.add_argument("--level", choices=LEVELS, default="postal")
    build_cmd.add_argument(
        "--cache", type=Path, default=Path.home() / ".cache" / "terrakit" / "sources"
    )
    build_cmd.add_argument("--out", type=Path, default=Path("datasets"))
    build_cmd.add_argument(
        "--repo-root", type=Path, default=Path(__file__).resolve().parents[3]
    )
    build_cmd.add_argument("--version", default=None, help="override dataset version")
    build_cmd.add_argument(
        "--skip-download", action="store_true", help="use cached sources only"
    )
    build_cmd.add_argument(
        "--no-names",
        action="store_true",
        help="skip alternateNamesV2 ingestion (localized, exonym and historic names)",
    )
    build_cmd.add_argument(
        "--max-name-rows",
        type=int,
        default=None,
        help="only read the first N rows of alternateNamesV2 (development aid; "
             "the resulting package is incomplete and must not be published)",
    )

    validate_cmd = sub.add_parser("validate", help="validate a built package")
    validate_cmd.add_argument("path", type=Path, help="package directory")
    validate_cmd.add_argument("--json", action="store_true")

    args = parser.parse_args(argv)

    if args.command == "build":
        return cmd_build(args)
    if args.command == "validate":
        return cmd_validate(args)
    return 1


def cmd_build(args) -> int:
    country = args.country.upper()
    repo_root = args.repo_root.resolve()
    level = args.level
    # Resolved before use: GeoNames publishes no postal file for some countries
    # (Georgia, an MVP country per spec section 48, returns 404). Demoting the
    # level is honest — the manifest declares 'cities', and the SDK reports the
    # postal feature unavailable instead of returning empty results that look
    # like the country has no postal codes.
    want_postal = level in ("postal", "full")
    want_names = not args.no_names

    print("[1/7] sources")
    if args.skip_download:
        print("  skipped (--skip-download), using cache")
        sources = _load_cached(
            args.cache,
            country,
            need_postal=want_postal,
            need_alternate_names=want_names,
        )
        # Postal is optional even here: an absent cache entry means the same
        # thing it means online, that this country has no upstream postal data.
        missing = {"countryInfo", "admin1", "admin2", "cities500"} - set(sources)
        if missing:
            print(
                f"error: cached sources missing for {sorted(missing)}; "
                f"run without --skip-download",
                file=sys.stderr,
            )
            return 2
    else:
        sources = fetch_all(
            args.cache,
            country,
            need_postal=want_postal,
            need_alternate_names=want_names,
        )

    for key, item in sorted(sources.items()):
        state = "cached" if item.from_cache else "downloaded"
        print(f"  {item.spec.filename}: {state}, {item.size:,} bytes")

    if want_postal and "postal" not in sources:
        print(
            f"  postal source unavailable for {country}; "
            f"building at level 'cities' instead"
        )
        want_postal = False
        level = "cities"

    print("\n[2/7] parse")
    countries = read_country_info(sources["countryInfo"].path)
    if country not in countries:
        print(f"error: {country} not found in countryInfo.txt", file=sys.stderr)
        return 2
    info = countries[country]
    admin1 = read_admin1(sources["admin1"].path, country)
    admin2 = read_admin2(sources["admin2"].path, country)
    settlements = (
        read_settlements(sources["cities500"].path, country)
        if level in ("cities", "postal", "full")
        else []
    )
    postal = (
        read_postal_zip(sources["postal"].path, country) if want_postal else []
    )
    print(f"  country: {info.name} (ISO {info.iso3} {info.numeric_code})")
    print(f"  admin1: {len(admin1)}, admin2: {len(admin2)}")
    print(f"  settlements: {len(settlements)}, postal rows: {len(postal)}")

    print("\n[3/7] normalize + deduplicate + build hierarchy")
    # Prefer the id: countryInfo gives the capital as a NAME and does not supply
    # one for every capital, while cities500 carries geonameids for all of them.
    capital_geonameid = None
    for record in settlements:
        if record.name == info.capital:
            capital_geonameid = record.geonameid
            break
    if capital_geonameid is None:
        print(f"  note: capital {info.capital!r} not in cities500; trying by name")

    build = build_hierarchy(
        country,
        capital_geonameid=capital_geonameid,
        admin1=admin1,
        admin2=admin2,
        settlements=settlements,
        postal=postal,
        from_normalize=normalize,
        from_postal_normalize=postal_normalize,
        country_name=info.name,
        country_geonameid=info.geonameid,
        # countryInfo.txt publishes no coordinates for a country, only for its
        # administrative areas. Leaving them None lets _backfill_admin_coordinates
        # derive the country centroid from its admin1 children, which is a better
        # centroid than inventing one from an area-average.
        country_latitude=None,
        country_longitude=None,
        country_population=info.population or None,
        capital_name=info.capital or None,
    )
    print(f"  places: {build.stats['places']}")
    print(
        "  by level: " + ", ".join(
            f"L{level_no}={build.stats.get(f'level_{level_no}', 0)}"
            for level_no in range(1, 8)
        )
    )
    print(
        "  by type: " + ", ".join(
            f"{name}={build.stats.get(f'type_{name}', 0)}"
            for name in ("country", "province", "state", "district", "city",
                         "town", "village", "neighborhood")
            if build.stats.get(f"type_{name}", 0)
        )
    )
    print(f"  closure rows: {build.stats['closure_rows']}")
    print(
        f"  identity: ISO3166-2={build.stats['identity_iso3166_2']}, "
        f"geonames={build.stats['identity_geonames']}, "
        f"hash={build.stats['identity_hash']}"
    )
    if want_postal:
        print(
            f"  postal: {build.stats['postal_codes']} codes, "
            f"{build.stats['postal_areas']} areas, "
            f"{build.stats['postal_multi_area']} covering >1 area"
        )
    for warning in build.warnings[:10]:
        print(f"  ! {warning}")

    # Before stage 4: name validity is evaluated against the run epoch, never
    # the wall clock, or a rebuild would change as names become current.
    epoch = default_source_date_epoch()

    print("\n[4/9] attach names")
    if "alternateNames" in sources:
        country_geonameids = {
            int(place.source_row_id)
            for place in build.places
            if place.source_row_id and place.source_row_id.isdigit()
        }
        country_language = primary_language_of(info.languages)
        print(
            f"  streaming alternateNamesV2 for {len(country_geonameids):,} places"
        )
        print(f"  primary language: {country_language or '(none declared)'}")

        # Script of each place's own name, so a romanization of a non-Latin place
        # can be told apart from a genuine exonym.
        primary_scripts = {
            int(place.source_row_id): detect_script(place.name)
            for place in build.places
            if place.source_row_id and place.source_row_id.isdigit()
        }

        alternate_rows, name_stats = read_alternate_names(
            sources["alternateNames"].path,
            country_geonameids=country_geonameids,
            primary_scripts=primary_scripts,
            valid_at=datetime.fromtimestamp(epoch, tz=UTC).date(),
            from_normalize=normalize,
            max_rows=args.max_name_rows,
            log=lambda message: print(f"  {message}"),
        )
        print(name_stats.render())

        build.place_names, build.alternate_names, attach_stats = attach_names(
            build.places,
            alternate_rows,
            country_language=country_language,
            from_normalize=normalize,
            country_geonameid=info.geonameid,
        )
        build.stats.update(attach_stats)
        print(
            f"  attached: {attach_stats['place_names']:,} localized names in "
            f"{attach_stats['languages']} languages, "
            f"{attach_stats['alternate_names']:,} searchable alternates"
        )
        print(
            f"    historic: {attach_stats['historic_names']:,}, "
            f"transliterations: {attach_stats['transliterations']:,}, "
            f"exonyms: {attach_stats['exonyms']:,}"
        )
        if attach_stats["display_names_promoted"]:
            print(
                "  country display name promoted to the local name "
                "(countryInfo publishes English)"
            )
    else:
        print("  alternateNamesV2 unavailable; no localized names")
        build.place_names = []
        build.alternate_names = []

    # The display name may have changed, so the search index and phonetic codes
    # are built from the post-attachment values. Build before this point and the
    # package indexes 'Turkey' while places.name says 'Türkiye'.
    country_row = next(
        (p for p in build.places if p.level == 1 and p.parent_id is None), None
    )
    if country_row is not None:
        print(f"  country row name: {country_row.name!r} "
              f"(name_norm={country_row.name_norm!r})")

    print("\n[5/9] validate (pre-build)")
    report = validate_build(build, country)
    print(report.render())
    if not report.ok:
        print("\nBuild aborted: validation errors present.", file=sys.stderr)
        return 1

    print("\n[6/9] write SQLite")
    # Dataset version derives from input content, so a rebuild from the same
    # sources yields the same version AND the same bytes.
    combined = "".join(sorted(item.sha256 for item in sources.values()))
    version = args.version or dataset_version_for(combined)
    package_name = f"terrakit-data-{country.lower()}"
    out_dir = args.out / f"{package_name}-{version}"

    currency_rows = _currency_rows(info.currency_code, info.currency_name)
    language_rows = _language_rows(info.languages)

    # Timezones actually referenced by this country's settlements, plus any
    # multi-zone country's well-known zones.
    tz_ids = {r.timezone_id for r in settlements if r.timezone_id}
    tz_ids.update(_country_timezone_ids(country))
    timezone_rows = _timezone_rows(tz_ids)

    continent_rows = [_continent(info.continent)]

    # The country row carries the display name the promotion step chose, so
    # `countries.name` and `places.name` agree. Leaving `countries.name` as
    # countryInfo's English value made `terra info TR` print "Turkey" while
    # `terra search Türkiye` returned the country — two answers to the same
    # question from one package.
    country_display_name = country_row.name if country_row is not None else info.name

    # Capital, resolved by name against the places the build actually contains.
    # Left NULL when unresolvable rather than guessed: a wrong capital in a
    # logistics application is worse than an absent one.
    capital_place_id = None
    if info.capital:
        target = normalize(info.capital)
        for place in build.places:
            if place.name_norm == target and place.level > 1:
                capital_place_id = place.id
                break
        if capital_place_id is None:
            print(
                f"  note: capital {info.capital!r} is not in this package; "
                f"countries.capital_place_id left empty"
            )

    country_rows = [
        (
            f"country:{country}", info.iso2, info.iso3, info.numeric_code,
            country_display_name, normalize(country_display_name),
            info.continent, capital_place_id,
            info.currency_code, f"+{info.phone}" if info.phone else None,
            info.tld or None, _driving_side(country),
            # The country's own coordinates, copied from the hierarchy anchor.
            # countryInfo publishes no country centroid, so _backfill derives
            # one from the administrative areas; `terra info` showed null while
            # `terra nearest` found the country, which is two answers to the same
            # question from one package.
            country_row.latitude if country_row is not None else None,
            country_row.longitude if country_row is not None else None,
            None, 1,
        )
    ]

    # Official languages, from countryInfo. The column order there is
    # priority order, and `terra info` shows them in it.
    country_language_rows = [
        (info.iso2, code, 1 if index == 0 else 0)
        for index, code in enumerate(_language_codes(info.languages))
    ]

    source_rows = [
        (
            item.spec.id, item.spec.id, item.spec.version or "unversioned",
            item.spec.license, item.spec.license_url, item.spec.attribution,
            item.spec.url, item.retrieved_at, item.sha256,
        )
        for key, item in sorted(sources.items())
    ]

    manifest_sources = [
        {
            "id": item.spec.id,
            "version": item.spec.version or "unversioned",
            "license": item.spec.license,
            "licenseUrl": item.spec.license_url,
            "attribution": item.spec.attribution,
            "url": item.spec.url,
            "retrievedAt": item.retrieved_at,
            "sha256": item.sha256,
        }
        for key, item in sorted(sources.items())
    ]

    metadata = PackageMetadata(
        name=package_name,
        version=version,
        country=country,
        level=level,
        generator_version=GENERATOR_VERSION,
        sources=manifest_sources,
        source_date_epoch=epoch,
    )

    db_path, manifest_path = build_package(
        out_dir=out_dir,
        repo_root=repo_root,
        metadata=metadata,
        build=build,
        currency_rows=currency_rows,
        language_rows=language_rows,
        timezone_rows=timezone_rows,
        continent_rows=continent_rows,
        country_rows=country_rows,
        country_language_rows=country_language_rows,
        source_rows=source_rows,
    )
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    size = db_path.stat().st_size
    print(f"  {db_path}")
    print(f"  {size:,} bytes ({size / 1024 / 1024:.1f} MB)")
    print(f"  {manifest_path}")

    print("\n[7/9] validate (written package)")
    package_report = validate_package(db_path)
    print(package_report.render())
    if not package_report.ok:
        print("\nPackage written but invalid; not publishable.", file=sys.stderr)
        return 1

    print("\n[8/9] summarize")
    print(f"  package: {out_dir}")
    print(f"  license: {manifest['license']}")
    print(
        f"  names:   {build.stats.get('place_names', 0):,} localized in "
        f"{build.stats.get('languages', 0)} languages, "
        f"{build.stats.get('alternate_names', 0):,} searchable alternates"
    )
    print(
        f"  history: {build.stats.get('historic_names', 0):,} historic, "
        f"{build.stats.get('transliterations', 0):,} transliterations, "
        f"{build.stats.get('exonyms', 0):,} exonyms"
    )
    print("  next: compress with zstd -19, sign manifest.json, publish to registry")
    print("\n[9/9] done")
    return 0


def cmd_validate(args) -> int:
    path: Path = args.path
    db_path = path / "data.sqlite" if path.is_dir() else path
    if not db_path.exists():
        print(f"error: no data.sqlite at {db_path}", file=sys.stderr)
        return 2

    report = validate_package(db_path)
    if args.json:
        import json

        print(
            json.dumps(
                {
                    "package": str(db_path),
                    "ok": report.ok,
                    "stats": report.stats,
                    "errors": [str(f) for f in report.errors],
                    "warnings": [str(f) for f in report.warnings],
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(report.render())
    return 0 if report.ok else 1


# --- reference data helpers -----------------------------------------------


def _continent(code: str) -> tuple[str, str, str]:
    names = {
        "EU": "Europe", "AS": "Asia", "AF": "Africa", "NA": "North America",
        "SA": "South America", "OC": "Oceania", "AN": "Antarctica",
    }
    name = names.get(code, code)
    return (code, name, normalize(name))


# IANA offsets, advisory only (DATA_MODEL.md section 10). The SDKs never use
# these; they return a real TimeZone/ZoneId.
_TIMEZONE_OFFSETS = {
    "Europe/Istanbul": 180,
    "Asia/Baghdad": 180,
    "Europe/London": 0,
    "Europe/Berlin": 60,
    "America/New_York": -300,
    "America/Los_Angeles": -480,
    "Asia/Tokyo": 540,
}


def _timezone_rows(ids) -> list[tuple[str, str, int]]:
    out = []
    for tz in sorted(i for i in ids if i):
        out.append((tz, tz, _TIMEZONE_OFFSETS.get(tz, 0)))
    return out


def _country_timezone_ids(country: str) -> list[str]:
    """
    Zones a country spans that may not appear in cities500.

    cities500 only lists timezones attached to settlements of 500+, so a
    sparsely-populated zone would be missing from the reference table while
    still being queried by `nearest`. The US is the case that matters for MVP.
    """
    return {
        "US": [
            "America/New_York", "America/Chicago", "America/Denver",
            "America/Los_Angeles", "America/Anchorage", "Pacific/Honolulu",
        ],
    }.get(country, [])


#: Right-hand traffic unless stated. ISO 3166 does not publish a driving-side
#: field, and GeoNames does not either, so this is the one place in the pipeline
#: where a value comes from outside the sources — and it is recorded as such rather
#: than presented as sourced data.
_RIGHT_HAND_DRIVING: frozenset[str] = frozenset({
    "AL", "AD", "AT", "BY", "BE", "BA", "BG", "HR", "CZ", "DK", "EE", "FI",
    "FR", "DE", "GR", "HU", "IS", "IT", "LV", "LI", "LT", "LU", "MT", "MD",
    "MC", "ME", "NL", "MK", "NO", "PL", "PT", "RO", "RU", "SM", "RS", "SK",
    "SI", "ES", "SE", "CH", "TR", "UA", "GB", "VA", "XK",
})
_LEFT_HAND_DRIVING: frozenset[str] = frozenset({
    "AG", "AU", "BS", "BB", "BZ", "BM", "BW", "CA", "DM", "FJ", "GD", "GM",
    "GH", "GD", "GY", "HK", "IN", "ID", "IE", "JM", "JP", "KE", "KI", "KN",
    "LS", "LR", "MO", "MS", "MU", "MV", "MW", "MY", "MZ", "NA", "NR", "NP",
    "NZ", "NI", "NE", "NG", "OM", "PK", "PG", "PY", "PE", "SC", "SL", "SB",
    "SO", "SR", "SZ", "TZ", "TH", "TL", "TO", "TT", "UG", "US", "VC", "WS",
    "YE", "ZA", "ZW",
})


def _driving_side(country_iso2: str) -> str | None:
    """
    Which side of the road traffic keeps to.

    Both sides occur, and getting it wrong sends a taxi the wrong way, so it is
    worth a column. Neither ISO 3166 nor GeoNames publishes the field, so the
    values come from a table in this file rather than from the sources — which is
    why `countries.driving_side` is NULL rather than wrong when a country is
    missing from it.
    """
    code = country_iso2.upper()
    if code in _RIGHT_HAND_DRIVING:
        return "right"
    if code in _LEFT_HAND_DRIVING:
        return "left"
    return None


def _language_codes(languages_field: str) -> list[str]:
    """
    CountryInfo's `languages` field, reduced to bare ISO 639-1 codes.

    'tr-TR,ku,diq,az,av' -> ['tr', 'ku', 'diq', 'az', 'av'].
    """
    return [
        token.split("-", 1)[0].strip().lower()
        for token in languages_field.split(",")
        if token.strip()
    ]


def _currency_rows(code: str, name: str) -> list[tuple[str, str, str | None, int]]:
    """
    ISO 4217. minor_units is the number of decimal places.

    The upstream dump gives the code and a bare name ('Lira'), so the symbol
    and decimal count come from a small local table. A missing entry is not an
    error: minor_units defaults to 2, which is right for almost every currency,
    and an absent symbol is honest rather than guessed.
    """
    symbols: dict[str, str] = {
        "TRY": "₺", "EUR": "€", "GBP": "£", "USD": "$",
        "GEL": "₾", "JPY": "¥",
    }
    minor_units: dict[str, int] = {
        "JPY": 0, "KRW": 0, "VND": 0, "CLP": 0, "ISK": 0,
        "BHD": 3, "KWD": 3, "JOD": 3, "OMR": 3, "TND": 3,
    }
    return [
        (
            code,
            name or code,
            symbols.get(code),
            minor_units.get(code, 2),
        )
    ]


def _language_rows(languages: str) -> list[tuple[str, str, str, str]]:
    iso3 = {
        "tr": ("tur", "Turkish"), "en": ("eng", "English"),
        "de": ("deu", "German"), "fr": ("fra", "French"),
        "ka": ("kat", "Georgian"), "ja": ("jpn", "Japanese"),
        "ru": ("rus", "Russian"), "ku": ("kur", "Kurdish"),
        "diq": ("div", "Dimli"), "az": ("aze", "Azerbaijani"),
        "av": ("ava", "Avaric"), "es": ("spa", "Spanish"),
        "it": ("ita", "Italian"), "nl": ("nld", "Dutch"),
    }
    out = []
    for code in sorted({c.strip() for c in languages.split(",") if c.strip()}):
        primary = code.split("-", 1)[0]
        three, name = iso3.get(primary, (primary, primary))
        out.append((primary, three, name, normalize(name)))
    return out


def _is_zip(path: Path) -> bool:
    with path.open("rb") as handle:
        return handle.read(4) == b"PK\x03\x04"


def _load_cached(
    cache_dir: Path,
    country: str,
    *,
    need_postal: bool,
    need_alternate_names: bool = True,
) -> dict:
    """Reconstruct source descriptors from the cache, without touching the network."""
    from datetime import UTC, datetime

    from .download import DownloadedSource, Downloader
    from .geonames import SOURCES

    downloader = Downloader(cache_dir)
    wanted = {
        "countryInfo": "countryInfo.txt",
        "admin1": "admin1CodesASCII.txt",
        "admin2": "admin2Codes.txt",
        "cities500": "cities500.txt",
    }
    if need_postal:
        wanted["postal"] = f"{country}.zip"
    if need_alternate_names:
        wanted["alternateNames"] = "alternateNamesV2.zip"

    out: dict[str, DownloadedSource] = {}
    for key, filename in wanted.items():
        path = cache_dir / filename
        if not path.exists():
            continue
        # A cached zip may be a saved HTTP error body (an earlier run before the
        # optional-404 handling). Same magic-number check as the downloader.
        if path.name.endswith(".zip") and not _is_zip(path):
            print(f"  dropping invalid cached archive {path}")
            continue
        spec = SOURCES[key]
        stat = path.stat()
        out[key] = DownloadedSource(
            spec=spec,
            path=path,
            sha256=downloader.sha256(path),
            size=stat.st_size,
            retrieved_at=datetime.fromtimestamp(
                stat.st_mtime, tz=UTC
            ).isoformat().replace("+00:00", "Z"),
            from_cache=True,
        )
    return out


if __name__ == "__main__":
    sys.exit(main())
