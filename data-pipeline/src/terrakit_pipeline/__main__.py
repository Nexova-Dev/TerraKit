"""
Pipeline entry point.

    python -m terrakit_pipeline build TR --level postal --out ../datasets
    python -m terrakit_pipeline validate ../datasets/terrakit-data-tr-2026.10.1

Stages, in SPEC section 27 order. Each prints what it did, so a build log is a
usable audit record rather than a status line.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .download import fetch_all
from .geonames import (
    read_admin1,
    read_admin2,
    read_country_info,
    read_postal_zip,
    read_settlements,
)
from .hierarchy import build_hierarchy
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

    print("[1/7] sources")
    if args.skip_download:
        print("  skipped (--skip-download), using cache")
        sources = _load_cached(args.cache, country, need_postal=want_postal)
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
        sources = fetch_all(args.cache, country, need_postal=want_postal)

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

    print("\n[4/7] validate (pre-build)")
    report = validate_build(build, country)
    print(report.render())
    if not report.ok:
        print("\nBuild aborted: validation errors present.", file=sys.stderr)
        return 1

    print("\n[5/7] write SQLite")
    epoch = default_source_date_epoch()
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

    country_rows = [
        (
            f"country:{country}", info.iso2, info.iso3, info.numeric_code,
            info.name, normalize(info.name), info.continent, None,
            info.currency_code, f"+{info.phone}" if info.phone else None,
            info.tld or None, None, None, None, None, 1,
        )
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
        source_rows=source_rows,
    )
    size = db_path.stat().st_size
    print(f"  {db_path}")
    print(f"  {size:,} bytes ({size / 1024 / 1024:.1f} MB)")
    print(f"  {manifest_path}")

    print("\n[6/7] validate (written package)")
    package_report = validate_package(db_path)
    print(package_report.render())
    if not package_report.ok:
        print("\nPackage written but invalid; not publishable.", file=sys.stderr)
        return 1

    print("\n[7/7] done")
    print(f"  package: {out_dir}")
    print(f"  license: CC-BY-4.0 (GeoNames)")
    print("  next: compress with zstd -19, sign manifest.json, publish to registry")
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


def _load_cached(cache_dir: Path, country: str, *, need_postal: bool) -> dict:
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
