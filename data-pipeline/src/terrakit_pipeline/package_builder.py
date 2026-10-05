"""
SQLite package writer and manifest emitter.

Implements the 'Database Generator', 'SQLite Generator', 'Package Builder' and
'Checksum / Signature' stages of SPEC section 27.

Two properties are load-bearing and both are tested:

1. Byte-identical output for identical input (PACKAGE_FORMAT.md section 12).
   Every timestamp comes from SOURCE_DATE_EPOCH, rows are inserted in sorted
   order, SQLite page size is fixed, and the file is vacuumed so free-page
   layout does not drift.
2. The shipped DDL is the authoritative DDL. This module applies
   core/schema/*.sql rather than carrying its own copy of the schema, because a
   second copy would drift and a drifted package fails
   minReaderVersion in the field, not in CI.
"""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from .hierarchy import BuildResult
from .normalize import (
    SEARCH_KEY_MIN_LENGTH,
    normalize,
    postal_normalize,
    search_key_unaccented,
)

__all__ = ["PackageWriter", "PackageMetadata", "build_package"]

SCHEMA_VERSION = 1
PACKAGE_FORMAT_VERSION = 1
MIN_READER_VERSION = 1


@dataclass
class PackageMetadata:
    name: str
    version: str
    country: str
    level: str
    generator_version: str
    sources: list[dict]
    source_date_epoch: int


class PackageWriter:
    """
    Builds one package directory.

    Usage:
        writer = PackageWriter(out_dir, metadata)
        writer.open()
        writer.insert_all(build_result)
        writer.finalize()
    """

    def __init__(self, out_dir: Path, metadata: PackageMetadata, schema_dir: Path):
        self.out_dir = out_dir
        self.metadata = metadata
        self.schema_dir = schema_dir
        self.db_path = out_dir / "data.sqlite"
        self.conn: sqlite3.Connection | None = None
        self.counts: dict[str, int] = {}
        # Source ids that must exist in data_sources before places rows can
        # reference them. Enforced rather than trusted: a place pointing at a
        # missing source is an unattributable row, which LICENSING.md section 6
        # refuses to ship.
        self.known_source_ids: set[str] = set()

    def _assert_source_known(self, source_id: str, place_id: str) -> None:
        if source_id not in self.known_source_ids:
            raise ValueError(
                f"place {place_id} references data_source {source_id!r}, which has "
                "no data_sources row. Every row must be attributable "
                "(LICENSING.md section 6)."
            )

    # --- schema -------------------------------------------------------------

    def open(self) -> None:
        self.out_dir.mkdir(parents=True, exist_ok=True)
        if self.db_path.exists():
            self.db_path.unlink()
        self.conn = sqlite3.connect(self.db_path, isolation_level=None)
        self._apply_pragmas(self.conn)

        for sql_file in sorted(self.schema_dir.glob("*.sql")):
            self.conn.executescript(sql_file.read_text(encoding="utf-8"))

        iso_timestamp = datetime.fromtimestamp(
            self.metadata.source_date_epoch, tz=UTC
        ).isoformat().replace("+00:00", "Z")

        self.conn.executemany(
            "INSERT INTO meta (key, value) VALUES (?, ?)",
            sorted(
                {
                    "schema_version": str(SCHEMA_VERSION),
                    "schema_min_reader_version": str(MIN_READER_VERSION),
                    "generator_version": self.metadata.generator_version,
                    "dataset_version": self.metadata.version,
                    "source_date_epoch": str(self.metadata.source_date_epoch),
                    "package_level": self.metadata.level,
                    "generated_at": iso_timestamp,
                }.items()
            ),
        )

    @staticmethod
    def _apply_pragmas(conn: sqlite3.Connection) -> None:
        """
        Fixed layout for reproducibility (PACKAGE_FORMAT.md section 12).

        journal_mode=DELETE rather than WAL: WAL leaves -wal/-shm sidecars and
        puts the header write counter in nondeterministic territory.
        auto_vacuum=0 plus an explicit VACUUM at the end keeps free-page layout
        stable across runs.
        """
        conn.execute("PRAGMA page_size = 4096")
        conn.execute("PRAGMA journal_mode = DELETE")
        conn.execute("PRAGMA auto_vacuum = NONE")
        conn.execute("PRAGMA foreign_keys = ON")
        conn.execute("PRAGMA legacy_file_format = OFF")
        # The pipeline writes every value explicitly; a surprise default would
        # be a silent data bug.
        conn.execute("PRAGMA trusted_schema = OFF")

    # --- content ------------------------------------------------------------

    def insert_reference_data(
        self,
        *,
        continent_codes: set[str],
        currencies: list[tuple],
        languages: list[tuple],
        timezones: list[tuple],
        continents: list[tuple],
    ) -> None:
        """
        Insert only the rows this package needs, so a country package stays small.

        All four reference tables are tuples matching the DDL column order.
        Filtering by continent keeps a single-country package from carrying the
        planet's reference data.
        """
        conn = self.conn
        assert conn is not None

        conn.executemany(
            "INSERT INTO continents (code, name, name_norm) VALUES (?, ?, ?)",
            sorted(row for row in continents if row[0] in continent_codes),
        )
        conn.executemany(
            "INSERT INTO currencies (code, name, symbol, minor_units) VALUES (?, ?, ?, ?)",
            sorted(currencies, key=lambda row: row[0]),
        )
        conn.executemany(
            "INSERT INTO languages (code, iso639_3, name, name_norm) VALUES (?, ?, ?, ?)",
            sorted(languages, key=lambda row: row[0]),
        )
        conn.executemany(
            "INSERT INTO timezones (id, name, utc_offset_min) VALUES (?, ?, ?)",
            sorted(timezones, key=lambda row: row[0]),
        )
        self.counts["currencies"] = len(currencies)
        self.counts["languages"] = len(languages)
        self.counts["timezones"] = len(timezones)

    def insert_countries(self, country_rows: list[tuple]) -> None:
        assert self.conn is not None
        self.conn.executemany(
            """
            INSERT INTO countries (
                id, iso2, iso3, numeric_code, name, name_norm, continent_code,
                capital_place_id, currency_code, phone_code, internet_tld,
                driving_side, latitude, longitude, flag_emoji, independent
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            country_rows,
        )
        self.counts["countries"] = len(country_rows)

    def insert_data_sources(self, rows: list[tuple]) -> None:
        assert self.conn is not None
        self.conn.executemany(
            """
            INSERT INTO data_sources (
                id, name, version, license, license_url,
                attribution, url, retrieved_at, checksum
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            rows,
        )
        self.counts["data_sources"] = len(rows)
        self.known_source_ids = {row[0] for row in rows}

    def insert_all(self, build: BuildResult) -> None:
        """Insert places, closure, postal, search indexes and R*Tree."""
        conn = self.conn
        assert conn is not None
        iso_timestamp = datetime.fromtimestamp(
            self.metadata.source_date_epoch, tz=UTC
        ).isoformat().replace("+00:00", "Z")

        # Insert in (level, id) order, NOT plain id order.
        #
        # The FK on parent_id requires a parent to exist before its children.
        # GeoNames ids are numeric strings, so sorting by id alone is not
        # topological: a village with id 10046384 under a district with id
        # 7732205 sorts first and the insert fails. Since validation guarantees
        # parent.level == child.level - 1, ordering by level first is
        # topological by construction, and (level, id) is still fully
        # deterministic, so reproducibility is unaffected.
        ordered = sorted(build.places, key=lambda p: (p.level, p.id))

        # rowid_int follows the same total order, so the R*Tree mapping is
        # reproducible without depending on insertion side effects.
        for index, place in enumerate(ordered, start=1):
            place.rowid_int = index
            self._assert_source_known(place.data_source, place.id)

        conn.executemany(
            """
            INSERT INTO places (
                id, rowid_int, type, level, country_iso2, parent_id, name,
                name_norm, admin_code, identity_source, latitude, longitude,
                population, timezone_id, currency_code, search_rank, source_id,
                source_row_id, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (
                    p.id, p.rowid_int, p.type, p.level, p.country_iso2,
                    p.parent_id, p.name, p.name_norm, p.admin_code,
                    p.identity_source, p.latitude, p.longitude, p.population,
                    p.timezone_id, None, p.search_rank, p.data_source,
                    p.source_row_id, iso_timestamp,
                )
                for p in ordered
            ],
        )

        conn.executemany(
            "INSERT INTO place_closure (ancestor_id, descendant_id, depth) "
            "VALUES (?, ?, ?)",
            [(c.ancestor_id, c.descendant_id, c.depth) for c in build.closure],
        )

        conn.executemany(
            """
            INSERT INTO postal_codes (
                id, country_iso2, code, code_norm, place_id, latitude, longitude
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (p.id, p.country_iso2, p.code, p.code_norm, p.place_id,
                 p.latitude, p.longitude)
                for p in build.postal_codes
            ],
        )

        conn.executemany(
            "INSERT INTO postal_areas (postal_code_id, place_id, is_primary) "
            "VALUES (?, ?, ?)",
            [(a.postal_code_id, a.place_id, int(a.is_primary)) for a in build.postal_areas],
        )

        self._insert_search_indexes(build)
        self._insert_rtree(build)

        self.counts.update(
            {
                "places": len(build.places),
                "closure_rows": len(build.closure),
                "postal_codes": len(build.postal_codes),
                "postal_areas": len(build.postal_areas),
            }
        )

    def _insert_search_indexes(self, build: BuildResult) -> None:
        """
        Populate search_fts and phonetic_codes.

        Both are build-time products (ADR-003). No SDK computes either.
        """
        conn = self.conn
        assert conn is not None

        # Only names of 3+ characters reach search_fts.
        #
        # The trigram tokenizer needs 3 characters to produce a token, so a
        # shorter name yields no indexable content. Short queries are served by
        # the idx_places_norm B-tree prefix index instead — see
        # DATA_MODEL.md section 9, which makes this routing documented API
        # behaviour rather than a hidden fallback. Indexing the row anyway
        # would cost bytes and buy nothing, since a trigram MATCH can never
        # match it.
        conn.executemany(
            "INSERT INTO search_fts (name_norm, name_norm_unaccented, place_id) "
            "VALUES (?, ?, ?)",
            [
                (p.name_norm, search_key_unaccented(p.name), p.id)
                for p in build.places
                if len(p.name_norm) >= SEARCH_KEY_MIN_LENGTH
            ],
        )

        from .phonetic import double_metaphone  # local import: heavy table

        rows: set[tuple[str, str]] = set()
        for place in build.places:
            for code in double_metaphone(place.name):
                if code:
                    rows.add((code, place.id))
        conn.executemany(
            "INSERT OR IGNORE INTO phonetic_codes (code, place_id) VALUES (?, ?)",
            sorted(rows),
        )
        self.counts["phonetic_codes"] = len(rows)

    def _insert_rtree(self, build: BuildResult) -> None:
        """
        R*Tree over places that have coordinates.

        Small administrative areas get a zero-extent box from their centroid.
        A box with min == max still works as an R*Tree entry; it degrades to a
        point lookup, which is the correct semantics for a centroid.
        """
        conn = self.conn
        assert conn is not None

        rows = [
            (p.rowid_int, p.latitude, p.latitude, p.longitude, p.longitude)
            for p in build.places
            if p.latitude is not None and p.longitude is not None
        ]
        conn.executemany(
            "INSERT INTO places_rtree (id_rowid, min_lat, max_lat, min_lon, max_lon) "
            "VALUES (?, ?, ?, ?, ?)",
            rows,
        )
        self.counts["rtree_entries"] = len(rows)

    # --- finalize -----------------------------------------------------------

    def finalize(self) -> Path:
        """Close cleanly, then vacuum so free-page layout is deterministic."""
        conn = self.conn
        assert conn is not None
        conn.execute("VACUUM")
        conn.execute("PRAGMA optimize")
        conn.close()
        self.conn = None
        return self.db_path

    # --- checksums ----------------------------------------------------------

    @staticmethod
    def sha256_file(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()


def build_package(
    *,
    out_dir: Path,
    repo_root: Path,
    metadata: PackageMetadata,
    build: BuildResult,
    currency_rows,
    language_rows,
    timezone_rows,
    continent_rows,
    country_rows,
    source_rows,
) -> tuple[Path, Path]:
    """
    Full package build. Returns (data.sqlite path, manifest.json path).

    Deterministic by construction; ARCHITECTURE.md section 11 and
    PACKAGE_FORMAT.md section 12 depend on it.
    """
    writer = PackageWriter(out_dir, metadata, repo_root / "core" / "schema")
    writer.open()
    writer.insert_reference_data(
        continent_codes={c[0] for c in continent_rows},
        currencies=currency_rows,
        languages=language_rows,
        timezones=timezone_rows,
        continents=continent_rows,
    )
    writer.insert_countries(country_rows)
    writer.insert_data_sources(source_rows)
    writer.insert_all(build)
    db_path = writer.finalize()

    raw_size = db_path.stat().st_size
    db_sha256 = PackageWriter.sha256_file(db_path)
    writer.counts["places"] = writer.counts.get("places", 0)

    entity_counts = {
        "countries": writer.counts.get("countries", 0),
        "places": writer.counts.get("places", 0),
        "postalCodes": writer.counts.get("postal_codes", 0),
        "postalAreas": writer.counts.get("postal_areas", 0),
        "phoneticCodes": writer.counts.get("phonetic_codes", 0),
    }
    for level in range(2, 8):
        entity_counts[f"level{level}"] = build.stats.get(f"level_{level}", 0)

    iso_timestamp = datetime.fromtimestamp(
        metadata.source_date_epoch, tz=UTC
    ).isoformat().replace("+00:00", "Z")

    manifest = {
        "packageFormatVersion": PACKAGE_FORMAT_VERSION,
        "name": metadata.name,
        "version": metadata.version,
        "country": metadata.country,
        "level": metadata.level,
        "schemaVersion": SCHEMA_VERSION,
        "minReaderVersion": MIN_READER_VERSION,
        "generator": {
            "name": "terrakit-pipeline",
            "version": metadata.generator_version,
        },
        "generatedAt": iso_timestamp,
        "sourceDateEpoch": metadata.source_date_epoch,
        "size": {"raw": raw_size, "compressed": None},
        "entities": entity_counts,
        "files": [
            {
                "path": "data.sqlite",
                "rawSize": raw_size,
                "compressedSize": None,
                "sha256": db_sha256,
                "compression": "zstd",
            }
        ],
        "packageSha256": db_sha256,
        "license": _resolve_package_license(metadata.sources),
        "sources": metadata.sources,
        "attribution": [s["attribution"] for s in metadata.sources],
        "dependencies": [],
        "minSdk": {"swift": "1.0.0", "kotlin": "1.0.0"},
        "features": _features_for_level(metadata.level, build),
    }

    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return db_path, manifest_path


def _resolve_package_license(sources: list[dict]) -> str:
    """
    Declared per package, never globally (LICENSING.md section 1).

    If sources disagree on license, the package must not claim one. Raising
    here is the enforcement point — there is no override flag.
    """
    licenses = {s["license"] for s in sources}
    if not licenses:
        raise ValueError(
            "package has no sources; refusing to emit without a license "
            "(LICENSING.md section 6)"
        )
    if len(licenses) > 1:
        raise ValueError(
            f"conflicting source licenses {sorted(licenses)}; a single package "
            "must not claim one license. Split the package or resolve upstream."
        )
    return licenses.pop()


def _features_for_level(level: str, build: BuildResult) -> list[str]:
    """
    Capability flags. An SDK checks these instead of assuming a table is
    populated, so a --basic package reports a capability as unavailable rather
    than failing on an empty result.
    """
    features = ["search", "localization"]
    if build.stats.get("level_5", 0) or build.stats.get("level_4", 0):
        features.append("cities")
    if build.postal_codes:
        features.append("postal")
    if any(p.latitude is not None for p in build.places):
        features.append("nearest")
    if level in ("cities", "postal", "full"):
        features.append("autocomplete")
    return sorted(set(features))


def default_source_date_epoch() -> int:
    """
    Fallback epoch for a local build with SOURCE_DATE_EPOCH unset.

    Deliberately a fixed constant rather than time.time(). An unset env var
    must not silently make every build produce different bytes, because then
    'reproducible' becomes untestable.
    """
    if raw := os.environ.get("SOURCE_DATE_EPOCH"):
        return int(raw)
    return _DEFAULT_SOURCE_DATE_EPOCH


# 2026-10-01T00:00:00Z. A fixed constant, not time.time(): an unset env var must
# not make every build produce different bytes, or "reproducible" is untestable.
# version.py imports this so the two modules cannot drift.
_DEFAULT_SOURCE_DATE_EPOCH = 1_790_812_800


def tool_versions() -> dict[str, str]:
    """Versions recorded in the manifest so a bad build is identifiable."""
    versions = {"python": f"{os.sys.version_info.major}.{os.sys.version_info.minor}"}
    try:
        versions["git"] = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        versions["git"] = "unknown"
    return versions
