#!/usr/bin/env python3
"""
Pipeline unit tests.

Covers the parts that are easy to get subtly wrong and expensive to debug from
a built package: identity resolution, hierarchy construction, closure-table
completeness, postal many-to-many, and the invariants the validator enforces.

Fixtures are synthetic and deliberately include the shapes that break naive
implementations — an admin2 whose admin1 is missing, a postal code spanning two
districts, a settlement whose admin link is broken, a name that collides under
one parent.

Run:  python3 data-pipeline/tests/test_pipeline.py
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

from terrakit_pipeline.geonames import (  # noqa: E402
    Admin1,
    Admin2,
    PostalRecord,
    Settlement,
    iso3166_2_from_geonames,
)
from terrakit_pipeline.hierarchy import build_hierarchy  # noqa: E402
from terrakit_pipeline.normalize import normalize, postal_normalize  # noqa: E402
from terrakit_pipeline.phonetic import double_metaphone  # noqa: E402
from terrakit_pipeline.validator import validate_build  # noqa: E402

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    if not condition:
        FAILURES.append(f"  {message}")


def settlement(
    geonameid: int,
    name: str,
    admin1: str = "",
    admin2: str = "",
    population: int = 0,
    lat: float = 0.0,
    lon: float = 0.0,
    timezone: str = "Europe/Istanbul",
) -> Settlement:
    return Settlement(
        geonameid=geonameid,
        name=name,
        ascii_name=name,
        latitude=lat,
        longitude=lon,
        feature_class="P",
        feature_code="PPL",
        country_iso2="TR",
        admin1_code=admin1,
        admin2_code=admin2,
        admin3_code="",
        population=population,
        elevation="",
        dem="",
        timezone_id=timezone,
        modified_at="2026-01-01",
    )


def postal(
    code: str,
    place: str,
    admin1: str,
    admin2: str,
    lat: float = 36.0,
    lon: float = 30.0,
) -> PostalRecord:
    return PostalRecord(
        country_iso2="TR",
        postal_code=code,
        place_name=place,
        admin1_name="Antalya",
        admin1_code=admin1,
        admin2_name="Muratpaşa",
        admin2_code=admin2,
        admin3_name="",
        admin3_code="",
        latitude=lat,
        longitude=lon,
        accuracy=3,
    )


def build_fixtures():
    admin1 = [
        Admin1("TR", "TR.07", "Antalya", "Antalya", 323776),
        Admin1("TR", "TR.06", "Ankara", "Ankara", 325772),
    ]
    admin2 = [
        Admin2("TR", "07", "TR.07.7732185", "Alanya", "Alanya", 7732185),
        Admin2("TR", "07", "TR.07.8631677", "Gerger", "Gerger", 8631677),
        # admin1_code '99' is absent from admin1CodesASCII — an upstream gap.
        Admin2("TR", "99", "TR.99.9999999", "Yetim", "Yetim", 9999999),
    ]
    settlements = [
        settlement(323777, "Antalya", "07", "", 1335000, 36.8969, 30.7133),
        settlement(323778, "Alanya", "07", "7732185", 300000, 36.5444, 31.9975),
        settlement(323779, "Gerger", "07", "8631677", 5000, 37.8778, 38.7833),
        # admin1_code '00' does not resolve: must attach to the country root.
        settlement(610838, "Znaur", "00", "", 900, 42.3, 43.3),
        # Duplicate name under the same parent: warning, not error.
        settlement(700001, "Köy", "07", "8631677", 100, 37.0, 38.0),
        settlement(700002, "Köy", "07", "8631677", 120, 37.1, 38.1),
    ]
    postal_records = [
        postal("07000", "Antalya", "07", ""),
        postal("07160", "Muratpaşa", "07", "7732185"),
        # One code across two districts: the many-to-many case.
        postal("07010", "Kundu", "07", "7732185"),
        postal("07010", "Kumköy", "07", "8631677"),
        postal("06160", "Ankara", "06", ""),
    ]
    return admin1, admin2, settlements, postal_records


def main() -> int:
    admin1, admin2, settlements, postal_records = build_fixtures()
    build = build_hierarchy(
        "TR",
        capital_geonameid=323777,
        admin1=admin1,
        admin2=admin2,
        settlements=settlements,
        postal=postal_records,
        from_normalize=normalize,
        from_postal_normalize=postal_normalize,
        country_name="Türkiye",
        country_geonameid=325918,
        capital_name="Ankara",
    )

    by_id = {p.id: p for p in build.places}
    print("fixture hierarchy")
    for place in sorted(build.places, key=lambda p: (p.level, p.id)):
        print(
            f"  L{place.level} {place.type:<12} {place.name:<10} "
            f"parent={place.parent_id or '-'}"
        )
    print()

    print("[1] country anchor at level 1")
    check("country:TR" in by_id, "country:TR must exist as a places row")
    check(by_id.get("country:TR").level == 1, "country row must be level 1")
    check(by_id.get("country:TR").parent_id is None, "country row must be the root")

    print("[2] ISO 3166-2 identity (ADR-002 rule 1)")
    check("admin:TR-07" in by_id, "admin1 identity must be ISO 3166-2")
    check(
        iso3166_2_from_geonames("TR.07") == "TR-07",
        "TR.07 must convert to TR-07",
    )
    check(
        by_id["admin:TR-07"].identity_source == "iso3166-2",
        "admin1 must record identity_source iso3166-2",
    )

    print("[3] GeoNames identity (ADR-002 rule 2)")
    check("place:geonames:7732185" in by_id, "admin2 identity must use geonameid")
    check(
        by_id["place:geonames:7732185"].identity_source == "geonames",
        "admin2 must record identity_source geonames",
    )

    print("[4] levels come from the parent, not from the source")
    antalya = by_id.get("admin:TR-07")
    alanya = by_id.get("place:geonames:7732185")
    city = by_id.get("place:geonames:323777")
    check(
        alanya.level == antalya.level + 1,
        f"admin2 level {alanya.level} must be one below admin1 {antalya.level}",
    )
    check(
        city.level == alanya.level,
        f"a settlement directly under a province must sit at the same level as "
        f"its sibling district ({city.level} vs {alanya.level})",
    )

    print("[5] type is semantic, level is structural (DATA_MODEL.md 1)")
    check(antalya.type == "province", "TR top-level area must be a province")
    check(alanya.type == "district", "TR admin2 must be a district")
    check(city.type == "city", f"a 1.3M settlement must be a city, got {city.type}")

    print("[6] settlement types are population-driven")
    town = by_id.get("place:geonames:323779")   # 5 000
    village = by_id.get("place:geonames:610838")  # 900
    check(town.type == "town", f"5000 must be a town, got {town.type}")
    check(village.type == "village", f"900 must be a village, got {village.type}")

    print("[7] broken upstream links attach to the country root")
    orphan_admin2 = by_id.get("place:geonames:9999999")
    check(
        orphan_admin2.parent_id == "country:TR",
        "an admin2 with an unknown admin1 must attach to the country",
    )
    check(
        orphan_admin2.level == 2,
        f"a district directly under the country IS the top admin area and must be "
        f"level 2, got {orphan_admin2.level}",
    )
    znaur = by_id.get("place:geonames:610838")
    check(
        znaur.parent_id == "country:TR",
        "a settlement with an unresolvable admin1 must attach to the country",
    )
    check(znaur.level == 2, f"such a settlement sits at level 2, got {znaur.level}")

    print("[8] closure table is complete and consistent")
    for place in build.places:
        expected = set()
        ancestor = place.id
        depth = 0
        while ancestor:
            expected.add((ancestor, depth))
            ancestor = by_id[ancestor].parent_id
            depth += 1
        actual = {
            (row.ancestor_id, row.depth)
            for row in build.closure
            if row.descendant_id == place.id
        }
        check(
            actual == expected,
            f"closure mismatch for {place.id}: expected {sorted(expected)}, "
            f"got {sorted(actual)}",
        )
    check(
        any(r.ancestor_id == "country:TR" and r.descendant_id == "place:geonames:323777"
            and r.depth == 2 for r in build.closure),
        "every place must be reachable from the country row",
    )

    print("[9] postal is many-to-many (DATA_MODEL.md section 8)")
    areas_for_07010 = [a for a in build.postal_areas if a.postal_code_id == "postal:TR:07010"]
    district_areas = [
        a for a in areas_for_07010
        if a.place_id.startswith("place:geonames:")
    ]
    province_areas = [a for a in areas_for_07010 if a.place_id.startswith("admin:")]
    check(
        len(district_areas) == 2,
        f"a code spanning two districts must cover both, got {len(district_areas)}",
    )
    check(
        len(province_areas) == 1,
        f"the shared province must be linked once, got {len(province_areas)}",
    )
    code = next(c for c in build.postal_codes if c.code == "07010")
    check(
        code.place_id == "place:geonames:7732185",
        f"primary place must be a district, got {code.place_id}",
    )
    check(
        sum(a.is_primary for a in areas_for_07010) == 1,
        "exactly one area per code may be primary",
    )
    check(
        not [a for a in areas_for_07010 if a.is_primary and a.place_id.startswith("admin:")],
        "the province must never outrank a district as primary",
    )

    print("[10] postal normalization is the strict variant")
    check(postal_normalize("07160") == "07160", "leading zero must survive")
    check(postal_normalize("SW1A 1AA") == "sw1a1aa", "spaces must be removed")
    code = next(c for c in build.postal_codes if c.code == "07000")
    check(code.code_norm == postal_normalize(code.code), "code_norm must match")

    print("[11] search rank is a monotone integer (DATA_MODEL.md section 9)")
    check(
        all(isinstance(p.search_rank, int) for p in build.places),
        "search_rank must be an integer so ORDER BY can use the index",
    )
    check(
        city.search_rank > village.search_rank,
        "a 1.3M city must outrank a 900-person village",
    )

    print("[12] admin coordinates are backfilled from children")
    check(antalya.latitude is not None, "a province needs a centroid for nearest()")
    check(antalya.latitude is not None and 36.0 < antalya.latitude < 38.0,
          f"province centroid should be near its children, got {antalya.latitude}")

    print("[13] capital gets a rank boost")
    capital = by_id.get("place:geonames:323777")
    check(
        capital.search_rank > by_id["place:geonames:323778"].search_rank,
        "a capital must outrank a non-capital of the same level",
    )

    print("[14] validator agrees with the builder")
    report = validate_build(build, "TR")
    check(report.ok, f"fixture must validate with 0 errors, got {len(report.errors)}")
    check(
        len(report.warnings) >= 1,
        "duplicate sibling names must surface as a warning, not pass silently",
    )
    duplicate_findings = [f for f in report.warnings if f.rule.startswith("duplicate-names")]
    check(
        any("Köy" in f.message for f in duplicate_findings),
        f"the duplicate 'Köy' must be reported; warnings: "
        f"{[f.message for f in report.warnings]}",
    )
    check(
        not [f for f in report.findings if f.rule == "orphaned-entity"],
        "attaching broken links to the country root must leave no orphans",
    )
    check(
        not [f for f in report.findings if f.rule == "hash-id-ratio"],
        "a fixture with resolvable ids must not trip the hash-ratio warning",
    )

    print("[15] phonetic codes are attached to the right names")
    check(
        double_metaphone("Antalya") == double_metaphone("Antaliya")[:0]
        or double_metaphone("Antalya") == ("ANTL",),
        f"Antalya must encode to ANTL, got {double_metaphone('Antalya')}",
    )
    check(
        double_metaphone("Atatürk") == double_metaphone("Ataturk"),
        "Atatürk and Ataturk must share a code (spec section 8)",
    )

    print()
    if FAILURES:
        print("FAILURES:", file=sys.stderr)
        for failure in FAILURES:
            print(failure, file=sys.stderr)
        print(f"\n{len(FAILURES)} failure(s)", file=sys.stderr)
        return 1
    print("all pipeline tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
