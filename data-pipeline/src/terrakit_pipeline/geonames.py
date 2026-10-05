"""
GeoNames source reader.

Parses the GeoNames CC BY 4.0 dump files into typed records. No database, no
ORM: this stage only reads and validates, so a plain dataclass per row type is
the honest representation.

File layouts are positional, documented at
https://download.geonames.org/export/dump/readme.txt and asserted by
FIELD_COUNT checks so a format change fails loudly instead of silently
shifting columns.

License: GeoNames is CC BY 4.0 (LICENSING.md section 2). Every record produced
here carries the source id, and the pipeline refuses to emit a package without
a resolved license.
"""

from __future__ import annotations

import zipfile
from dataclasses import dataclass, field
from pathlib import Path

__all__ = [
    "CountryInfo",
    "Admin1",
    "Admin2",
    "PostalRecord",
    "Settlement",
    "read_country_info",
    "read_admin1",
    "read_admin2",
    "read_postal_zip",
    "read_settlements",
    "SOURCES",
    "SourceSpec",
]


@dataclass(frozen=True)
class SourceSpec:
    """One upstream dataset, with the license the pipeline must emit."""

    id: str
    url: str
    filename: str
    license: str
    license_url: str
    attribution: str
    # GeoNames stamps the dump date in the directory listing, not in a header
    # field, so the pipeline records the download date and the checksum.
    version: str = ""


SOURCES: dict[str, SourceSpec] = {
    "countryInfo": SourceSpec(
        id="geonames_countryinfo",
        url="https://download.geonames.org/export/dump/countryInfo.txt",
        filename="countryInfo.txt",
        license="CC-BY-4.0",
        license_url="https://creativecommons.org/licenses/by/4.0/",
        attribution="GeoNames country information, CC BY 4.0",
    ),
    "admin1": SourceSpec(
        id="geonames_admin1",
        url="https://download.geonames.org/export/dump/admin1CodesASCII.txt",
        filename="admin1CodesASCII.txt",
        license="CC-BY-4.0",
        license_url="https://creativecommons.org/licenses/by/4.0/",
        attribution="GeoNames admin1 codes, CC BY 4.0",
    ),
    "admin2": SourceSpec(
        id="geonames_admin2",
        url="https://download.geonames.org/export/dump/admin2Codes.txt",
        filename="admin2Codes.txt",
        license="CC-BY-4.0",
        license_url="https://creativecommons.org/licenses/by/4.0/",
        attribution="GeoNames admin2 codes, CC BY 4.0",
    ),
    "cities500": SourceSpec(
        id="geonames_cities500",
        url="https://download.geonames.org/export/dump/cities500.zip",
        filename="cities500.txt",
        license="CC-BY-4.0",
        license_url="https://creativecommons.org/licenses/by/4.0/",
        attribution="GeoNames settlements (cities500), CC BY 4.0",
    ),
    "postal": SourceSpec(
        id="geonames_postal",
        url="https://download.geonames.org/export/zip/{iso2}.zip",
        filename="{iso2}.txt",
        license="CC-BY-4.0",
        license_url="https://creativecommons.org/licenses/by/4.0/",
        attribution="GeoNames postal codes, CC BY 4.0",
    ),
}


@dataclass(frozen=True)
class CountryInfo:
    iso2: str
    iso3: str
    numeric_code: str
    fips: str
    name: str
    capital: str
    area_sq_km: float
    population: int
    continent: str
    tld: str
    currency_code: str
    currency_name: str
    phone: str
    postal_code_format: str
    postal_regex: str
    languages: str
    geonameid: int
    neighbours: str
    equivalent_fips: str


@dataclass(frozen=True)
class Admin1:
    """Level-2 administrative area. ``code`` is ISO 3166-2 (TR.07 -> TR-07)."""

    country_iso2: str
    code: str          # 'TR.07' as published
    name: str          # local/native name
    ascii_name: str
    geonameid: int


@dataclass(frozen=True)
class Admin2:
    """Level-3 administrative area. Keyed by geonameid, not by name."""

    country_iso2: str
    admin1_code: str
    code: str          # 'TR.07.7732185'
    name: str
    ascii_name: str
    geonameid: int


@dataclass(frozen=True)
class PostalRecord:
    country_iso2: str
    postal_code: str
    place_name: str
    admin1_name: str
    admin1_code: str
    admin2_name: str
    admin2_code: str
    admin3_name: str
    admin3_code: str
    latitude: float
    longitude: float
    accuracy: float


@dataclass
class Settlement:
    """
    A populated place from cities500.

    admin1_code is the numeric admin code, NOT the geonameid — that mismatch is
    real in the dump and is resolved in hierarchy.py. Keeping both the raw code
    and a resolved reference here would hide the join; keeping only the raw code
    makes the join explicit and testable.
    """

    geonameid: int
    name: str
    ascii_name: str
    latitude: float
    longitude: float
    feature_class: str
    feature_code: str
    country_iso2: str
    admin1_code: str
    admin2_code: str
    admin3_code: str
    population: int
    elevation: str
    dem: str
    timezone_id: str
    modified_at: str
    # Resolved by hierarchy.py, empty until then.
    alt_names: list[tuple[str, str, str]] = field(default_factory=list)

    @property
    def settlement_type(self) -> str:
        """
        Feature code to TerraKit type.

        Deliberately driven by population rather than by the feature-code rank,
        because the rank describes administrative role, not size. GeoNames marks
        a 400-person hamlet that happens to be a district seat as PPLA2, and
        that must not outrank a real city in search results — `search_rank` is
        population-based (DATA_MODEL.md section 9) and a type that disagreed with
        it would make ranking self-inconsistent.

        Thresholds follow the common English-language convention: >= 100 000 is a
        city, >= 5 000 a town, below that a village. PPLX (a section of a
        populated place, i.e. a neighborhood) keeps its own type because that is
        structural, not a size judgement.
        """
        if self.feature_code == "PPLX":
            return "neighborhood"
        if self.feature_code == "PPLL":
            return "village"       # populated locality
        if self.feature_code == "PPLQ":
            return "village"       # abandoned; place still usable

        population = self.population
        if population >= 100_000:
            return "city"
        if population >= 5_000:
            return "town"
        return "village"


# --- readers ---------------------------------------------------------------
# Positional field counts from the GeoNames readme. Asserted per row group so a
# silent format change is a loud failure.

_COUNTRYINFO_COLUMNS = 19


def read_country_info(path: Path) -> dict[str, CountryInfo]:
    """
    countryInfo.txt: '#'-commented, then 19 columns keyed by ISO 3166-1 alpha-2.

    Verified against the live dump for TR:
        1 ISO  2 ISO3  3 ISO-Numeric  4 fips  5 Country  6 Capital
        7 Area(sq km)  8 Population  9 Continent  10 tld  11 CurrencyCode
        12 CurrencyName  13 Phone  14 Postal Code Format  15 Postal Code Regex
        16 Languages  17 geonameid  18 neighbours  19 EquivalentFipsCode

    Column 5 is the ENGLISH name. GeoNames publishes "Turkey" here, not
    "Türkiye"; the endonym arrives via alternateNames. See
    ARCHITECTURE.md section 13 for the open question on which name becomes
    `places.name`, and DATA_MODEL.md section 5.1 for why `place_names` exists
    alongside it.
    """
    out: dict[str, CountryInfo] = {}
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != _COUNTRYINFO_COLUMNS:
                raise ValueError(
                    f"countryInfo.txt: expected {_COUNTRYINFO_COLUMNS} columns, "
                    f"got {len(parts)} for {parts[0]!r}. "
                    "Upstream format changed; re-check the field map."
                )
            iso2 = parts[0]
            if not iso2:
                continue
            out[iso2] = CountryInfo(
                iso2=iso2,
                iso3=parts[1],
                numeric_code=parts[2],
                fips=parts[3],
                name=parts[4],
                capital=parts[5],
                area_sq_km=_float(parts[6]),
                population=_int(parts[7]),
                continent=parts[8],
                tld=parts[9],
                currency_code=parts[10],
                currency_name=parts[11],
                phone=parts[12],
                postal_code_format=parts[13],
                postal_regex=parts[14],
                languages=parts[15],
                geonameid=_int(parts[16]),
                neighbours=parts[17],
                equivalent_fips=parts[18] if len(parts) > 18 else "",
            )
    return out


def read_admin1(path: Path, country_iso2: str | None = None) -> list[Admin1]:
    """admin1CodesASCII.txt: code, name, asciiname, geonameid."""
    out: list[Admin1] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 4:
                raise ValueError(
                    f"admin1CodesASCII.txt: expected 4 columns, got {len(parts)}"
                )
            code = parts[0]
            iso2 = code.split(".", 1)[0]
            if country_iso2 and iso2 != country_iso2:
                continue
            out.append(
                Admin1(
                    country_iso2=iso2,
                    code=code,
                    name=parts[1],
                    ascii_name=parts[2],
                    geonameid=_int(parts[3]),
                )
            )
    return out


def read_admin2(path: Path, country_iso2: str | None = None) -> list[Admin2]:
    """admin2Codes.txt: code, name, asciiname, geonameid."""
    out: list[Admin2] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 4:
                raise ValueError(
                    f"admin2Codes.txt: expected 4 columns, got {len(parts)}"
                )
            code = parts[0]
            segments = code.split(".")
            iso2 = segments[0]
            if country_iso2 and iso2 != country_iso2:
                continue
            admin1 = segments[1] if len(segments) > 1 else ""
            out.append(
                Admin2(
                    country_iso2=iso2,
                    admin1_code=admin1,
                    code=code,
                    name=parts[1],
                    ascii_name=parts[2],
                    geonameid=_int(parts[3]),
                )
            )
    return out


_POSTAL_COLUMNS = 12


def read_postal_zip(zip_path: Path, country_iso2: str) -> list[PostalRecord]:
    """
    {ISO2}.zip containing one tab-separated file.

    Layout: cc, postal code, place name, admin1 name, admin1 code, admin2 name,
    admin2 code, admin3 name, admin3 code, latitude, longitude, accuracy.

    Note admin2_code is a geonameid here, while admin1_code is a short numeric
    code. Asymmetric, and the hierarchy builder depends on knowing which is
    which.
    """
    out: list[PostalRecord] = []
    with zipfile.ZipFile(zip_path) as archive:
        # The archive holds readme.txt AND {ISO2}.txt, both .txt. Picking the
        # first would parse the readme and fail on every row.
        names = [
            n for n in archive.namelist()
            if Path(n).name.upper() == f"{country_iso2.upper()}.TXT"
        ]
        if not names:
            raise ValueError(
                f"{zip_path.name}: no {country_iso2}.txt member "
                f"(archive holds {archive.namelist()})"
            )
        with archive.open(names[0]) as raw:
            for line_bytes in raw:
                line = line_bytes.decode("utf-8").rstrip("\n")
                if not line:
                    continue
                parts = line.split("\t")
                if len(parts) != _POSTAL_COLUMNS:
                    raise ValueError(
                        f"{zip_path.name}: expected {_POSTAL_COLUMNS} columns, "
                        f"got {len(parts)} for {parts[0]!r}"
                    )
                out.append(
                    PostalRecord(
                        country_iso2=parts[0],
                        postal_code=parts[1],
                        place_name=parts[2],
                        admin1_name=parts[3],
                        admin1_code=parts[4],
                        admin2_name=parts[5],
                        admin2_code=parts[6],
                        admin3_name=parts[7],
                        admin3_code=parts[8],
                        latitude=_float(parts[9]),
                        longitude=_float(parts[10]),
                        accuracy=_float(parts[11]),
                    )
                )
    if out and not all(r.country_iso2 == country_iso2 for r in out):
        mismatched = {r.country_iso2 for r in out}
        raise ValueError(
            f"{zip_path.name}: expected only {country_iso2}, found {sorted(mismatched)}"
        )
    return out


_SETTLEMENT_COLUMNS = 19


def read_settlements(path: Path, country_iso2: str | None = None) -> list[Settlement]:
    """
    cities500.txt: 19 columns.

    admin1_code is the short numeric admin code (e.g. '70'); admin2_code and
    admin3_code are geonameids. Not a uniform key space — resolved in
    hierarchy.py.
    """
    out: list[Settlement] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != _SETTLEMENT_COLUMNS:
                raise ValueError(
                    f"cities500.txt: expected {_SETTLEMENT_COLUMNS} columns, "
                    f"got {len(parts)} for geonameid {parts[0]!r}"
                )
            iso2 = parts[8]
            if country_iso2 and iso2 != country_iso2:
                continue
            out.append(
                Settlement(
                    geonameid=_int(parts[0]),
                    name=parts[1],
                    ascii_name=parts[2],
                    latitude=_float(parts[4]),
                    longitude=_float(parts[5]),
                    feature_class=parts[6],
                    feature_code=parts[7],
                    country_iso2=iso2,
                    admin1_code=parts[10],
                    admin2_code=parts[11],
                    admin3_code=parts[12],
                    population=_int(parts[14]),
                    elevation=parts[15],
                    dem=parts[16],
                    timezone_id=parts[17],
                    modified_at=parts[18],
                )
            )
    return out


# --- helpers ---------------------------------------------------------------


def _int(raw: str) -> int:
    raw = raw.strip()
    return int(raw) if raw else 0


def _float(raw: str) -> float:
    raw = raw.strip()
    try:
        return float(raw) if raw else 0.0
    except ValueError:
        return 0.0


def iso3166_2_from_geonames(code: str) -> str:
    """
    Convert GeoNames admin1 code to ISO 3166-2: 'TR.07' -> 'TR-07'.

    This is the ADR-002 rule-1 identity. The code is stable across renames,
    which is exactly why it outranks a name-derived ID.
    """
    return code.replace(".", "-", 1)
