"""
Package validator.

Implements SPEC section 30 and DATA_MODEL.md section 13. The same rules back
`terra validate` in the CLI and the blocking gate in this pipeline.

Severity is meaningful and not cosmetic:

- ERROR   blocks publication. The package cannot ship with one.
- WARNING surfaces to the user with a count, as SPEC section 30 requires.

The last two rules in DATA_MODEL.md section 13 exist to make section 29
enforceable rather than aspirational: a package that cannot prove where its data
came from is not shippable.
"""

from __future__ import annotations

import json
import re
import sqlite3
import unicodedata
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

from .normalize import SEARCH_KEY_MIN_LENGTH, normalize, postal_normalize

__all__ = ["Finding", "ValidationReport", "validate_package", "validate_build"]

ERROR = "error"
WARNING = "warning"


@dataclass(frozen=True)
class Finding:
    severity: str
    rule: str
    message: str
    context: str = ""

    def __str__(self) -> str:
        prefix = "✗" if self.severity == ERROR else "!"
        location = f" [{self.context}]" if self.context else ""
        return f"{prefix} [{self.rule}] {self.message}{location}"


@dataclass
class ValidationReport:
    findings: list[Finding] = field(default_factory=list)
    stats: dict[str, int] = field(default_factory=dict)

    @property
    def errors(self) -> list[Finding]:
        return [f for f in self.findings if f.severity == ERROR]

    @property
    def warnings(self) -> list[Finding]:
        return [f for f in self.findings if f.severity == WARNING]

    @property
    def ok(self) -> bool:
        return not self.errors

    def add(self, severity: str, rule: str, message: str, context: str = "") -> None:
        self.findings.append(Finding(severity, rule, message, context))

    def render(self) -> str:
        """
        SPEC section 30's output shape.

        Counts are by TYPE for settlements and by LEVEL for admin areas.
        Reporting 'Cities' as level 5 would print 0 for Türkiye, where the
        biggest cities sit at level 3 — a correct package with a report that
        claims there are no cities in it.
        """
        lines: list[str] = []
        for label, count_key in (
            ("Country structure", "countries"),
            ("Hierarchy anchor (level 1)", "level_1"),
            ("Administrative areas (level 2)", "level_2"),
            ("Districts (level 3)", "level_3"),
            ("Cities", "type_city"),
            ("Towns", "type_town"),
            ("Villages", "type_village"),
            ("Neighborhoods", "type_neighborhood"),
            ("Postal codes", "postal_codes"),
            ("Postal areas", "postal_areas"),
            ("Places with coordinates", "places_with_coords"),
        ):
            count = self.stats.get(count_key, 0)
            mark = "✓" if count > 0 else "—"
            lines.append(f"{mark} {label}: {count}")

        if self.findings:
            lines.append("")
            for finding in self.findings[:200]:
                lines.append(f"  {finding}")
            if len(self.findings) > 200:
                lines.append(f"  ... {len(self.findings) - 200} more")

        lines.append("")
        lines.append(f"Errors: {len(self.errors)}")
        lines.append(f"Warnings: {len(self.warnings)}")
        return "\n".join(lines)


def validate_build(build, country_iso2: str) -> ValidationReport:
    """Pre-insert checks. Cheap, and it fails before touching the disk."""
    report = ValidationReport()
    report.stats = dict(build.stats)

    ids: set[str] = set()
    for place in build.places:
        if place.id in ids:
            report.add(ERROR, "duplicate-ids", f"duplicate ID {place.id}")
        ids.add(place.id)

        if not place.name.strip():
            report.add(ERROR, "missing-required-field", "empty name", place.id)

        if not _is_well_formed(place.name):
            report.add(
                ERROR, "invalid-unicode",
                "name is not well-formed Unicode or contains control characters",
                place.id,
            )

        if place.name_norm != normalize(place.name):
            report.add(
                ERROR, "denormalization-drift",
                f"name_norm {place.name_norm!r} != normalize({place.name!r})",
                place.id,
            )

        # Only the country row (level 1) may be parentless. Everything else
        # detached from the hierarchy is a genuine orphan worth reporting.
        if place.parent_id is None and place.level > 1:
            report.add(
                WARNING, "orphaned-entity",
                f"{place.type} at level {place.level} has no parent",
                place.id,
            )

        if place.latitude is not None and not (-90 <= place.latitude <= 90):
            report.add(ERROR, "invalid-coordinates", f"latitude {place.latitude}", place.id)
        if place.longitude is not None and not (-180 <= place.longitude <= 180):
            report.add(ERROR, "invalid-coordinates", f"longitude {place.longitude}", place.id)
        if (
            place.latitude is not None
            and place.longitude is not None
            and place.latitude == 0.0
            and place.longitude == 0.0
            and place.type not in ("country",)
        ):
            report.add(
                WARNING, "invalid-coordinates",
                "coordinates are exactly (0, 0), which is almost always upstream junk",
                place.id,
            )

        if place.identity_source not in ("iso3166-2", "geonames", "osm", "hash"):
            report.add(
                ERROR, "identity-source",
                f"unknown identity_source {place.identity_source!r}", place.id,
            )

    # Broken hierarchy: parent must exist and be exactly one level shallower.
    by_id = {p.id: p for p in build.places}
    for place in build.places:
        if place.parent_id is None:
            continue
        parent = by_id.get(place.parent_id)
        if parent is None:
            report.add(
                ERROR, "missing-parent-ids",
                f"parent {place.parent_id} does not exist", place.id,
            )
        elif parent.level != place.level - 1:
            report.add(
                ERROR, "broken-hierarchy",
                f"level {place.level} under parent at level {parent.level}",
                place.id,
            )

    # Closure completeness.
    expected: dict[tuple[str, str], int] = {}
    for place in build.places:
        ancestor = place.id
        depth = 0
        seen: set[str] = set()
        while ancestor is not None and ancestor not in seen:
            seen.add(ancestor)
            expected[(ancestor, place.id)] = depth
            ancestor = by_id[ancestor].parent_id if ancestor in by_id else None
            depth += 1
    actual = {(c.ancestor_id, c.descendant_id): c.depth for c in build.closure}
    missing = set(expected) - set(actual)
    extra = set(actual) - set(expected)
    if missing:
        report.add(
            ERROR, "closure-completeness",
            f"{len(missing)} closure rows missing, e.g. {sorted(missing)[:3]}",
        )
    if extra:
        report.add(
            ERROR, "closure-completeness",
            f"{len(extra)} unexpected closure rows, e.g. {sorted(extra)[:3]}",
        )
    for key in set(expected) & set(actual):
        if expected[key] != actual[key]:
            report.add(
                ERROR, "closure-completeness",
                f"depth {actual[key]} != {expected[key]} for {key}",
            )

    # Hash-derived identity rate. Above the threshold means the source is too
    # weak to support stable identity (DATA_MODEL.md section 3.3).
    total = len(build.places) or 1
    hash_ratio = build.stats.get("identity_hash", 0) / total
    if hash_ratio > 0.005:
        report.add(
            WARNING, "hash-id-ratio",
            f"{hash_ratio:.2%} of places use hash-derived IDs "
            "(threshold 0.5%); source identity quality is low",
        )

    # Duplicate names within the same parent and type. Warning, not error,
    # because SPEC section 30 lists "duplicate names" and real data has them;
    # identical coordinates on top is what makes it an error.
    by_parent_type: dict[tuple, list] = defaultdict(list)
    for place in build.places:
        by_parent_type[(place.parent_id, place.type, place.name_norm)].append(place)
    for key, group in by_parent_type.items():
        if len(group) > 1:
            same_coords = {
                (p.latitude, p.longitude) for p in group
            } - {(None, None)}
            rule = "duplicate-names"
            message = f"{len(group)} places share the name {group[0].name!r} under the same parent"
            if len(same_coords) == 1 and len(same_coords) > 0:
                rule = "duplicate-names-coincident"
                message += " and have identical coordinates (likely duplicates)"
            report.add(
                ERROR if rule.endswith("coincident") else WARNING,
                rule, message, key[0] or country_iso2,
            )

    # Postal integrity.
    postal_ids = {p.id for p in build.postal_codes}
    place_ids = {p.id for p in build.places}
    for code in build.postal_codes:
        if code.code_norm != postal_normalize(code.code):
            report.add(
                ERROR, "postal-normalization",
                f"code_norm {code.code_norm!r} != postal_normalize({code.code!r})",
                code.id,
            )
        if code.place_id and code.place_id not in place_ids:
            report.add(
                ERROR, "postal-areas", f"primary place {code.place_id} missing", code.id
            )
    for area in build.postal_areas:
        if area.postal_code_id not in postal_ids:
            report.add(ERROR, "postal-areas", "unknown postal code", area.postal_code_id)
        if area.place_id not in place_ids:
            report.add(ERROR, "postal-areas", "unknown place", area.place_id)

    report.stats.setdefault("countries", 1)
    report.stats["places_with_coords"] = sum(
        1 for p in build.places if p.latitude is not None
    )
    report.stats["postal_codes"] = len(build.postal_codes)
    report.stats["postal_areas"] = len(build.postal_areas)
    # Settlements can sit at any level, so counts are by type (see render()).
    for type_name in ("city", "town", "village", "neighborhood"):
        report.stats[f"type_{type_name}"] = sum(
            1 for p in build.places if p.type == type_name
        )
    return report


def validate_package(db_path: Path) -> ValidationReport:
    """
    Post-build checks against the written file.

    Separate from validate_build on purpose: this is what `terra validate`
    runs on a package a user already has, including one downloaded from
    somewhere else.
    """
    report = ValidationReport()
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        tables = {
            row["name"]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type IN ('table','view')"
            )
        }
        required = {"meta", "countries", "places", "place_closure", "data_sources"}
        for missing in sorted(required - tables):
            report.add(ERROR, "schema", f"missing required table {missing}")
        if report.errors:
            return report

        meta = {row["key"]: row["value"] for row in conn.execute("SELECT * FROM meta")}
        for key in ("schema_version", "dataset_version", "generator_version"):
            if key not in meta:
                report.add(ERROR, "meta", f"missing meta key {key}")
        report.stats["schema_version"] = int(meta.get("schema_version", 0))

        report.stats["countries"] = conn.execute(
            "SELECT COUNT(*) FROM countries"
        ).fetchone()[0]
        report.stats["places"] = conn.execute("SELECT COUNT(*) FROM places").fetchone()[0]
        for level in range(1, 8):
            report.stats[f"level_{level}"] = conn.execute(
                "SELECT COUNT(*) FROM places WHERE level = ?", (level,)
            ).fetchone()[0]
        report.stats["postal_codes"] = conn.execute(
            "SELECT COUNT(*) FROM postal_codes"
        ).fetchone()[0]
        report.stats["places_with_coords"] = conn.execute(
            "SELECT COUNT(*) FROM places WHERE latitude IS NOT NULL"
        ).fetchone()[0]
        report.stats["postal_areas"] = conn.execute(
            "SELECT COUNT(*) FROM postal_areas"
        ).fetchone()[0]
        for type_name in ("city", "town", "village", "neighborhood"):
            report.stats[f"type_{type_name}"] = conn.execute(
                "SELECT COUNT(*) FROM places WHERE type = ?", (type_name,)
            ).fetchone()[0]

        # LICENSING.md section 6: attribution present and complete.
        sources = list(conn.execute("SELECT * FROM data_sources"))
        if not sources:
            report.add(
                ERROR, "attribution",
                "no data_sources rows; a package that cannot prove its origin "
                "is not shippable (LICENSING.md section 6)",
            )
        for source in sources:
            for field_name in ("license", "license_url", "attribution", "url", "retrieved_at"):
                if not (source[field_name] or "").strip():
                    report.add(
                        ERROR, "attribution",
                        f"source {source['id']} has empty {field_name}",
                        source["id"],
                    )

        # Duplicate IDs cannot occur (primary key) but a duplicate name at the
        # same parent can, and it matters for address disambiguation.
        duplicates = list(
            conn.execute(
                """
                SELECT parent_id, type, name_norm, COUNT(*) AS n
                FROM places
                GROUP BY parent_id, type, name_norm
                HAVING n > 1
                LIMIT 50
                """
            )
        )
        for row in duplicates:
            report.add(
                WARNING, "duplicate-names",
                f"{row['n']} places named {row['name_norm']!r} under the same parent",
                row["parent_id"] or "-",
            )

        # R*Tree coverage: anything with coordinates must be in the spatial index,
        # or `nearest` silently cannot find it.
        indexed = conn.execute("SELECT COUNT(*) FROM places_rtree").fetchone()[0]
        with_coords = report.stats["places_with_coords"]
        if indexed != with_coords:
            report.add(
                ERROR, "spatial-coverage",
                f"{with_coords} places have coordinates but only {indexed} are in "
                "places_rtree; nearest-location queries would silently miss some",
            )

        # Search index coverage.
        fts_rows = conn.execute("SELECT COUNT(*) FROM search_fts").fetchone()[0]
        searchable = conn.execute(
            "SELECT COUNT(*) FROM places WHERE LENGTH(name_norm) >= ?",
            (SEARCH_KEY_MIN_LENGTH,),
        ).fetchone()[0]
        if fts_rows != searchable:
            report.add(
                ERROR, "search-coverage",
                f"{searchable} searchable places but {fts_rows} rows in search_fts",
            )
    finally:
        conn.close()

    return report


def _is_well_formed(text: str) -> bool:
    """
    SPEC section 30 'invalid Unicode'.

    Rejects control characters and the soft hyphen. Soft hyphen is invisible and
    survives NFKD, so it silently breaks lookups in pasted text; catching it at
    build time is cheaper than chasing a missing search result.
    """
    if "­" in text:
        return False
    return not any(unicodedata.category(char) == "Cc" for char in text)
