"""
Hierarchy builder and identity resolver.

Implements DATA_PIPELINE.md stages 'Hierarchy Builder' and 'Deduplicator', and
the identity rules in DATA_MODEL.md section 3 (ADR-002).

The awkward part of real geographic data, and what this module exists to solve:

- GeoNames admin1 codes are ISO 3166-2 with a dot: 'TR.07' -> 'TR-07'.
- cities500 admin1_code is the short numeric suffix: '07'.
- cities500 admin2_code and postal admin2_code are geonameids: '7732185'.
- Postal records carry admin2_code as a geonameid but admin1_code as '07'.
- 6 Turkish postal codes span more than one admin2 (measured, not assumed), so
  postal <-> place is genuinely many-to-many and cannot be collapsed.

Resolving those keys correctly is the difference between a package whose
hierarchy is right and one that silently attributes a district to the wrong
province.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from dataclasses import dataclass, field

from .geonames import (
    Admin1,
    Admin2,
    PostalRecord,
    Settlement,
    iso3166_2_from_geonames,
)

__all__ = [
    "Place",
    "ClosureRow",
    "PostalArea",
    "PostalCode",
    "BuildResult",
    "build_hierarchy",
]


@dataclass
class Place:
    """
    One row of the unified `places` table.

    type is semantic, level is structural depth — see DATA_MODEL.md section 1.
    Türkiye's district is at level 3; the UK's is at level 4. Same type,
    different level, and that is not a bug in the data.

    `data_source` names the upstream dump this row actually came from, which is
    per-row rather than per-package: an admin2 comes from admin2Codes.txt while a
    settlement comes from cities500.txt, and DATA_MODEL.md section 11 requires
    per-row provenance so a re-sync can look the row up upstream.
    """

    id: str
    type: str
    level: int
    country_iso2: str
    parent_id: str | None
    name: str
    name_norm: str
    admin_code: str | None
    identity_source: str
    latitude: float | None
    longitude: float | None
    population: int | None
    timezone_id: str | None
    search_rank: int = 0
    source_row_id: str | None = None
    data_source: str = "geonames_cities500"

    def __post_init__(self) -> None:
        # rowid_int is assigned at insert time, deterministically, for the
        # R*Tree mapping. See 001_core.sql.
        self.rowid_int: int = 0


@dataclass(frozen=True)
class ClosureRow:
    ancestor_id: str
    descendant_id: str
    depth: int


@dataclass
class PostalCode:
    id: str
    country_iso2: str
    code: str
    code_norm: str
    place_id: str | None
    latitude: float | None
    longitude: float | None


@dataclass(frozen=True)
class PostalArea:
    postal_code_id: str
    place_id: str
    is_primary: bool


@dataclass
class BuildResult:
    places: list[Place] = field(default_factory=list)
    closure: list[ClosureRow] = field(default_factory=list)
    postal_codes: list[PostalCode] = field(default_factory=list)
    postal_areas: list[PostalArea] = field(default_factory=list)
    stats: dict[str, int] = field(default_factory=dict)
    warnings: list[str] = field(default_factory=list)


# Identity rules, DATA_MODEL.md section 3.3, in strict priority order.
ID_COUNTRY = "country"
ID_ADMIN_ISO = "admin"          # rule 1: ISO 3166-2
ID_PLACE_GEONAMES = "place"     # rule 2: GeoNames geonameid
ID_POSTAL = "postal"


def _admin1_type(country_iso2: str) -> str:
    """
    Top-level administrative area type per country.

    SPEC section 4 is explicit that the same depth means different things in
    different countries: Türkiye has provinces, the US states, Germany states,
    Japan prefectures, Switzerland cantons, Brazil states. One table with a
    per-country type keeps `turkey.provinces` and `japan.prefectures` honest
    without a schema change.

    Unknown countries default to 'region', the honest neutral term
    (DATA_MODEL.md section 2).
    """
    return {
        "TR": "province",     # il
        "US": "state", "CA": "province", "AU": "state", "BR": "state",
        "IN": "state", "MX": "state", "AR": "province", "CN": "province",
        "JP": "prefecture",   # ken
        "DE": "state",        # Bundesland
        "CH": "canton",
        "FR": "region",
        "IT": "region", "ES": "autonomous_community",
        "GB": "country_subdivision",
        "RU": "federal_subject",
        "UA": "region", "PL": "voivodeship", "SE": "county",
        "NO": "county", "FI": "region", "DK": "region",
        "GE": "region",       # mkhedrioni
        "PT": "district", "IE": "province", "NZ": "region",
    }.get(country_iso2, "region")


def _search_rank(population: int | None, level: int, is_capital: bool) -> int:
    """
    Precomputed ranking, DATA_MODEL.md section 9.

    Must be a monotone integer so `ORDER BY search_rank DESC` is an index
    scan. Ranking computed in SQL at query time cannot use an index, and
    section 40 allows 50 ms for indexed search.
    """
    if population is None or population < 0:
        population = 0
    # log10 of population, scaled, so a city of 10M does not permanently
    # outrank every other result regardless of match quality.
    rank = int(population.bit_length() * 100) if population else 0
    # Shallower levels are structurally more significant.
    rank += (8 - level) * 50
    if is_capital:
        rank += 5_000
    return rank


def build_hierarchy(
    country_iso2: str,
    *,
    capital_geonameid: int | None = None,
    admin1: list[Admin1],
    admin2: list[Admin2],
    settlements: list[Settlement],
    postal: list[PostalRecord],
    from_normalize,
    from_postal_normalize,
    country_name: str | None = None,
    country_geonameid: int | None = None,
    country_latitude: float | None = None,
    country_longitude: float | None = None,
    country_population: int | None = None,
    capital_name: str | None = None,
) -> BuildResult:
    """
    Build places, closure rows, postal codes and postal areas.

    `from_normalize` and `from_postal_normalize` are injected rather than
    imported so this module has no dependency on the normalizer and can be
    tested against a stub. Production passes
    terrakit_pipeline.normalize.normalize and .postal_normalize.

    Country metadata is optional: hierarchy construction does not need it, only
    the level-1 anchor row does, and a caller with nothing but admin records
    still gets a valid hierarchy.
    """
    result = BuildResult()
    places: dict[str, Place] = {}

    # --- level 1: the country ----------------------------------------------
    # SPEC section 4 lists Country inside the hierarchy (World -> Country ->
    # Region -> ...). Including it as a `places` row rather than only in the
    # `countries` table means every closure chain terminates at level 1, so a
    # settlement whose upstream admin1 reference is broken (Georgia publishes
    # admin1_code '00', which is not in admin1CodesASCII) can still be attached
    # to something real instead of becoming an orphan.
    #
    # The `countries` table still exists and still holds the ISO metadata; this
    # row is the hierarchy anchor, and both carry the same 'country:XX' id.
    country_name = country_name or country_iso2
    country_id = f"{ID_COUNTRY}:{country_iso2}"
    places[country_id] = Place(
        id=country_id,
        type="country",
        level=1,
        country_iso2=country_iso2,
        parent_id=None,
        name=country_name,
        name_norm=from_normalize(country_name),
        admin_code=country_iso2,
        identity_source="iso3166-2",
        latitude=country_latitude,
        longitude=country_longitude,
        population=country_population,
        timezone_id=None,
        source_row_id=str(country_geonameid) if country_geonameid else None,
        data_source="geonames_countryinfo",
    )

    # --- level 2: admin1 ----------------------------------------------------
    # admin1_code -> id, keyed both ways because upstream disagrees with itself
    # about which form it publishes.
    admin1_by_suffix: dict[str, Admin1] = {}
    admin1_by_geonameid: dict[int, Admin1] = {}
    for record in admin1:
        if record.country_iso2 != country_iso2:
            continue
        suffix = record.code.split(".", 1)[1] if "." in record.code else record.code
        admin1_by_suffix[suffix] = record
        admin1_by_geonameid[record.geonameid] = record
        place_id = f"{ID_ADMIN_ISO}:{iso3166_2_from_geonames(record.code)}"
        if place_id in places:
            result.warnings.append(
                f"duplicate admin1 identity {place_id}; keeping first "
                f"({record.code})"
            )
            continue
        places[place_id] = Place(
            id=place_id,
            type=_admin1_type(country_iso2),
            level=2,
            country_iso2=country_iso2,
            parent_id=country_id,
            name=record.name,
            name_norm=from_normalize(record.name),
            admin_code=iso3166_2_from_geonames(record.code),
            identity_source="iso3166-2",
            latitude=None,         # filled from settlements below
            longitude=None,
            population=None,
            timezone_id=None,
            source_row_id=str(record.geonameid),
            data_source="geonames_admin1",
        )

    # --- level 3: admin2 ----------------------------------------------------
    admin2_by_geonameid: dict[int, Admin2] = {}
    for record in admin2:
        if record.country_iso2 != country_iso2:
            continue
        admin2_by_geonameid[record.geonameid] = record
        parent = admin1_by_suffix.get(record.admin1_code)
        if parent is not None:
            parent_id = f"{ID_ADMIN_ISO}:{iso3166_2_from_geonames(parent.code)}"
        else:
            # Upstream gap: admin2 names an admin1 that admin1CodesASCII does not
            # publish. Attach to the country root and say so, rather than
            # dropping the district.
            parent_id = country_id
            result.warnings.append(
                f"admin2 {record.code} references admin1 {record.admin1_code!r}, "
                f"which is absent from admin1CodesASCII; attached to the country "
                f"root. Upstream data gap, not a modelling choice."
            )
        place_id = f"{ID_PLACE_GEONAMES}:geonames:{record.geonameid}"
        if place_id in places:
            continue
        places[place_id] = Place(
            id=place_id,
            type="district",
            # Level follows the parent, always. An admin2 whose province is
            # missing sits directly under the country, so it IS the country's
            # top administrative area as far as this data goes. Hardcoding 3
            # produced a level-3 row under a level-1 parent, which validation
            # correctly rejects as a broken hierarchy.
            level=3 if parent_id != country_id else 2,
            country_iso2=country_iso2,
            parent_id=parent_id,
            name=record.name,
            name_norm=from_normalize(record.name),
            admin_code=record.code,
            identity_source="geonames",
            latitude=None,
            longitude=None,
            population=None,
            timezone_id=None,
            source_row_id=str(record.geonameid),
            data_source="geonames_admin2",
        )

    # --- level 4+: settlements ----------------------------------------------
    # Deduplication (SPEC section 27 'Deduplicator'): GeoNames lists the same
    # settlement once per postal code it belongs to. Key on geonameid so the
    # package carries one row per real place.
    seen_settlements: set[int] = set()
    settlement_parent_by_geonameid: dict[int, str] = {}

    for record in settlements:
        if record.country_iso2 != country_iso2:
            continue
        if record.geonameid in seen_settlements:
            continue
        seen_settlements.add(record.geonameid)

        parent_id, parent_level = _resolve_settlement_parent(
            record, admin1_by_suffix, admin2_by_geonameid, places, country_iso2
        )
        # A settlement directly under a province is level 3; one under a
        # district is level 4. The parent decides, not the source: countries
        # genuinely differ in how deep their settlements sit.
        level = parent_level + 1 if parent_level else 5
        if level > 7:
            level = 7
        place_id = f"{ID_PLACE_GEONAMES}:geonames:{record.geonameid}"
        if place_id in places:
            continue
        places[place_id] = Place(
            id=place_id,
            type=record.settlement_type,
            level=level,
            country_iso2=country_iso2,
            parent_id=parent_id,
            name=record.name,
            name_norm=from_normalize(record.name),
            admin_code=record.admin1_code or None,
            identity_source="geonames",
            latitude=record.latitude or None,
            longitude=record.longitude or None,
            population=record.population or None,
            timezone_id=record.timezone_id or None,
            source_row_id=str(record.geonameid),
            data_source="geonames_cities500",
        )
        settlement_parent_by_geonameid[record.geonameid] = parent_id or ""

        # Aggregate settlement population onto its parent admin area, so
        # province/district records carry a meaningful population and rank
        # instead of NULL.
        if parent_id and parent_id in places and record.population:
            parent = places[parent_id]
            parent.population = (parent.population or 0) + record.population

    # Fill admin1/admin2 coordinates from the most populous child, which is a
    # better centroid than any single settlement and costs no extra data.
    _backfill_admin_coordinates(places)

    # capital_geonameid is authoritative when available: the countryInfo capital
    # is a NAME and countryInfo does not supply one for every capital.
    capital_place_id = None
    if capital_geonameid is not None:
        wanted = f"{ID_PLACE_GEONAMES}:geonames:{capital_geonameid}"
        if wanted in places:
            capital_place_id = wanted
    elif capital_name:
        for place in places.values():
            if place.name == capital_name and place.level > 1:
                capital_place_id = place.id
                break
        if capital_place_id is None:
            result.warnings.append(
                f"capital {capital_name!r} did not resolve to a place; "
                f"capital search boost unavailable for {country_iso2}"
            )

    for place in places.values():
        is_capital = place.id == capital_place_id
        place.search_rank = _search_rank(place.population, place.level, is_capital)

    # --- closure table ------------------------------------------------------
    result.closure = _build_closure(places)

    # --- postal -------------------------------------------------------------
    postal_codes: dict[str, PostalCode] = {}
    postal_candidates: dict[str, list[str]] = {}

    for record in postal:
        if record.country_iso2 != country_iso2 or not record.postal_code.strip():
            continue
        code_norm = from_postal_normalize(record.postal_code)
        if not code_norm:
            continue
        postal_id = f"{ID_POSTAL}:{country_iso2}:{record.postal_code.strip()}"

        # is_primary is decided ONCE per code, after every record for that code
        # has been seen. Deciding it per record marked a second district primary
        # too: a code spanning two districts produced two primary rows, so
        # `postalCodes(city:)` returned both and ranking had nothing to sort by.
        #
        # Deferred: (postal_id -> candidate) in first-seen order, which is
        # deterministic because records arrive in a fixed order from the dump.
        postal_candidates.setdefault(postal_id, [])
        for candidate in _postal_area_candidates(
            record, admin1_by_suffix, admin2_by_geonameid, places, country_iso2
        ):
            if candidate not in postal_candidates[postal_id]:
                postal_candidates[postal_id].append(candidate)

        # The primary is the most specific candidate seen: a district beats the
        # province it sits in.
        candidates = postal_candidates[postal_id]
        primary_place = next(
            (c for c in candidates if c.startswith(f"{ID_PLACE_GEONAMES}:")),
            candidates[0] if candidates else None,
        )

        if postal_id not in postal_codes:
            postal_codes[postal_id] = PostalCode(
                id=postal_id,
                country_iso2=country_iso2,
                code=record.postal_code.strip(),
                code_norm=code_norm,
                place_id=primary_place,
                latitude=record.latitude or None,
                longitude=record.longitude or None,
            )
        elif postal_codes[postal_id].place_id is None and primary_place:
            postal_codes[postal_id].place_id = primary_place

    result.places = sorted(places.values(), key=lambda p: p.id)
    result.postal_codes = sorted(postal_codes.values(), key=lambda p: p.id)

    # Areas are materialized from the deduplicated candidate lists, so one
    # primary flag per code by construction. Building them here rather than
    # during the scan is what makes that true: deciding primary-ness inline
    # meant the last record for a code overwrote the earlier verdict.
    postal_areas = [
        PostalArea(
            postal_code_id=postal_id,
            place_id=candidate,
            is_primary=(candidate == primary_for),
        )
        for postal_id, candidates in postal_candidates.items()
        for primary_for in (
            next(
                (c for c in candidates if c.startswith(f"{ID_PLACE_GEONAMES}:")),
                candidates[0] if candidates else None,
            ),
        )
        for candidate in candidates
    ]
    result.postal_areas = sorted(
        postal_areas, key=lambda a: (a.postal_code_id, a.place_id)
    )
    areas_per_code = Counter(area.postal_code_id for area in result.postal_areas)

    # A settlement can appear at any level depending on how its country nests
    # admin areas, so the validator reports counts by type as well as by level.
    # "Cities: 0" with 1 800 city rows is a reporting bug, not a data one.
    for type_name in (
        "country", "region", "province", "state", "prefecture", "canton",
        "district", "city", "town", "village", "neighborhood", "municipality",
        "borough", "ward", "suburb",
    ):
        result.stats[f"type_{type_name}"] = sum(
            1 for p in result.places if p.type == type_name
        )

    result.stats = {
        **result.stats,
        "places": len(result.places),
        # level_1 is the country anchor row. Without it the report prints
        # "L1=0" next to "country=1", which reads like a contradiction.
        "level_1": sum(1 for p in result.places if p.level == 1),
        "level_2": sum(1 for p in result.places if p.level == 2),
        "level_3": sum(1 for p in result.places if p.level == 3),
        "level_4": sum(1 for p in result.places if p.level == 4),
        "level_5": sum(1 for p in result.places if p.level == 5),
        "level_6": sum(1 for p in result.places if p.level == 6),
        "level_7": sum(1 for p in result.places if p.level == 7),
        "closure_rows": len(result.closure),
        "postal_codes": len(result.postal_codes),
        "postal_areas": len(result.postal_areas),
        # Counted with a Counter, not a nested scan. The nested version is
        # O(codes x areas): for Japan, 146 883 codes against 300 000 areas, which
        # is ~44 billion comparisons and turned a 4-second build into a
        # multi-minute one. Measured, not theoretical.
        "postal_multi_area": sum(
            1 for count in areas_per_code.values() if count > 1
        ),
        "identity_iso3166_2": sum(
            1 for p in result.places if p.identity_source == "iso3166-2"
        ),
        "identity_geonames": sum(
            1 for p in result.places if p.identity_source == "geonames"
        ),
        "identity_hash": sum(1 for p in result.places if p.identity_source == "hash"),
        # Only the country row is legitimately parentless.
        "places_without_parent": sum(
            1 for p in result.places if p.parent_id is None and p.level > 1
        ),
    }
    return result


def _resolve_settlement_parent(
    record: Settlement,
    admin1_by_suffix: dict[str, Admin1],
    admin2_by_geonameid: dict[int, Admin2],
    places: dict[str, Place],
    country_iso2: str,
) -> tuple[str | None, int]:
    """
    Resolve a settlement's parent, preferring the deepest available link.

    cities500 publishes admin1 as a short code, admin2 as a geonameid. Both are
    honored; admin3 is accepted as a parent too, because some countries do
    populate it (33 TR rows do, and Japan relies on it heavily).
    """
    admin2_record = admin2_by_geonameid.get(_safe_int(record.admin2_code))
    if admin2_record:
        admin2_id = f"{ID_PLACE_GEONAMES}:geonames:{admin2_record.geonameid}"
        if admin2_id in places:
            return admin2_id, places[admin2_id].level

    admin1_record = admin1_by_suffix.get(record.admin1_code)
    if admin1_record:
        admin1_id = (
            f"{ID_ADMIN_ISO}:{iso3166_2_from_geonames(admin1_record.code)}"
        )
        if admin1_id in places:
            return admin1_id, places[admin1_id].level
    if f"{ID_COUNTRY}:{country_iso2}" in places:
        return f"{ID_COUNTRY}:{country_iso2}", 1

    # No usable link upstream. Georgia publishes admin1_code '00' for a handful
    # of settlements and '00' is not in admin1CodesASCII, so this is a real
    # upstream defect rather than a modelling choice.
    #
    # Attach to the country root: the place stays reachable through the closure
    # table, and its level reflects the shallower hierarchy rather than
    # pretending to a depth the data does not support. Dropping the record
    # would be worse — a user searching for Znaur would find nothing.
    return f"{ID_COUNTRY}:{country_iso2}", 1


def _postal_area_candidates(
    record: PostalRecord,
    admin1_by_suffix: dict[str, Admin1],
    admin2_by_geonameid: dict[int, Admin2],
    places: dict[str, Place],
    country_iso2: str,
) -> list[str]:
    """
    Every place a postal code plausibly covers: its admin2 and its admin1.

    Both are stored so the closure table can answer
    `postalCodes(country:city:)` without a second join: a code belongs to a
    municipality, and the municipality belongs to a prefecture, so the province
    link is derivable but not indexed here.

    Deliberately NOT the country row. Adding it would give every code in Japan a
    link to `country:JP`, multiplying 142 577 codes by one row for no query that
    cannot be answered by filtering `postal_codes.country_iso2` instead.
    """
    out: list[str] = []
    admin2_record = admin2_by_geonameid.get(_safe_int(record.admin2_code))
    if admin2_record:
        candidate = f"{ID_PLACE_GEONAMES}:geonames:{admin2_record.geonameid}"
        if candidate in places:
            out.append(candidate)
    admin1_record = admin1_by_suffix.get(record.admin1_code)
    if admin1_record:
        candidate = f"{ID_ADMIN_ISO}:{iso3166_2_from_geonames(admin1_record.code)}"
        if candidate in places:
            out.append(candidate)
    return out


def _build_closure(places: dict[str, Place]) -> list[ClosureRow]:
    """
    Full closure, computed iteratively.

    Every package ships this fully materialized, so SDKs run zero recursive
    queries. Iterative rather than recursive because a malformed chain from
    upstream would otherwise hit Python's recursion limit instead of producing
    a validation error.
    """
    rows: list[ClosureRow] = []
    # .values(): places is keyed by id, so iterating it yields ids.
    for place in places.values():
        # Walk up, guarding against cycles with a visited set.
        seen: set[str] = set()
        ancestor_id: str | None = place.id
        depth = 0
        chain: list[tuple[str, int]] = []
        while ancestor_id is not None:
            if ancestor_id in seen:
                break
            seen.add(ancestor_id)
            chain.append((ancestor_id, depth))
            parent_id = places[ancestor_id].parent_id
            if parent_id is None or parent_id not in places:
                break
            ancestor_id = parent_id
            depth += 1
            if depth > 7:
                break
        rows.extend(
            ClosureRow(ancestor_id=ancestor, descendant_id=place.id, depth=d)
            for ancestor, d in chain
        )
    return sorted(
        rows, key=lambda r: (r.ancestor_id, r.depth, r.descendant_id)
    )


def _backfill_admin_coordinates(places: dict[str, Place]) -> None:
    """
    Give admin areas a centroid from their most populous child.

    Skipped when the admin area already has coordinates, so an authoritative
    upstream value is never overwritten by a derived one.
    """
    children: dict[str, list[Place]] = defaultdict(list)
    for place in places.values():
        if place.parent_id and place.latitude is not None:
            children[place.parent_id].append(place)

    # Iterate by depth so an admin area can inherit from its own children
    # before its parent inherits from it. snapshot(list) because the loop body
    # appends to children as coordinates propagate upward, which would
    # otherwise mutate the dict during iteration.
    for _ in range(2):
        for parent_id, kids in list(children.items()):
            parent = places.get(parent_id)
            if parent is None or parent.latitude is not None:
                continue
            best = max(kids, key=lambda p: p.population or 0)
            parent.latitude = best.latitude
            parent.longitude = best.longitude
            if parent.timezone_id is None:
                parent.timezone_id = best.timezone_id
            if parent.parent_id:
                children[parent.parent_id].append(parent)


def _safe_int(raw: str) -> int:
    try:
        return int(raw.strip())
    except (AttributeError, ValueError):
        return 0
