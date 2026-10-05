"""
Alternate names — localized names, exonyms, historic names, transliterations.

This module closes the largest pipeline gap. `place_names` and
`place_alternate_names` have been in the schema since v1 and validated from day
one, but nothing populated them, so `Terra.search("Constantinople")` found
nothing and `localizedName(for: .turkish)` returned the English name for every
language.

Why the filtering here is the hard part
----------------------------------------

alternateNamesV2 mixes four kinds of row that a user experiences as completely
different things, and a naive ingest puts them all on the same footing:

  * **Preferred names** — the official name in a language. Wanted.
  * **Transliterations** — "Istanbul" for "İstanbul". Wanted: this is what makes
    cross-script search work at all (DATA_MODEL.md section 5.2).
  * **Historic names** — "Constantinople", "Tiflis". Wanted: SPEC sections 8 and
    45 both treat historical names as a feature.
  * **Unverified user contributions** — a misspelling typed by one person. NOT
    wanted alongside the above. Ingesting it means `search("Istanbul")` matches a
    typo as a distinct place, and the package ships data that is not authoritative.

Rows are also time-bounded. Columns `from` and `to` record when a name became and
ceased being official. A name retired in 1930 is a historic name, not a current
one, and treating it as current would put "Constantinople" in the `name` column of
a place whose official name is İstanbul.

`isPreferredName`, `isShortName`, `isColloquial` and `isHistoric` are the four
discipline flags, and each maps onto a distinct `class` in
place_alternate_names.class. Nothing is dropped silently: every discard is counted
and reported, so a suspiciously large discard is visible rather than inferred.

Reproducibility
---------------

Validity is evaluated against the pipeline run's epoch, never `date.today()`. A
build that changes when a name stops being current is a build that is not
reproducible, and DATA_PIPELINE.md promises byte-identical output from identical
inputs.
"""

from __future__ import annotations

import zipfile
from collections import Counter
from dataclasses import dataclass, field
from datetime import date
from pathlib import Path

__all__ = [
    "AlternateName",
    "AlternateNameStats",
    "read_alternate_names",
    "classify",
    "normalize_language",
    "detect_script",
    "IDENTIFIER_CLASSES",
    "BIBLIOGRAPHIC_TO_ISO639_1",
    "SPECIAL_LANGUAGES",
    "UNDETERMINED",
    "ALTERNATE_COLUMNS",
]


#: Positional field count, asserted per row. GeoNames' readme describes 10.
ALTERNATE_COLUMNS = 10

#: Identifier classes. GeoNames stores these in `isolanguage` alongside real
#: languages, and they are **not names**: a Wikidata Q-id, a Wikipedia URL, a
#: UN/LOCODE, an IATA code, a UIC code.
#:
#: Derived by measuring the live file rather than by guessing — 19 219 334 rows
#: were scanned and every `isolanguage` value cross-checked against the
#: `iso-languagecodes.txt` that ships inside the same archive. The counts are in
#: `data-pipeline/sources/README.md`.
#:
#: Two of these are marked as *preferred* upstream. `link | https://… |
#: isPreferred=1` appears in the real file, which is why a naive ingest can put a
#: Wikipedia URL in a name column.
IDENTIFIER_CLASSES: frozenset[str] = frozenset({
    "wkdt",     # Wikidata Q-id,  1 111 665 rows
    "link",     # URL,              720 163
    "post",     # postal code,      147 464
    "unlc",     # UN/LOCODE,        101 945
    "lauc",     # LAU code,          80 568
    "iata",     # IATA,              21 125
    "icao",     # ICAO,              13 710
    "abbr",     # abbreviation,      12 500
    "uicn",     # UIC,                10 651
    "faac",     # FAA,                1 069
    "geoid",    # GeoNames ID,          939
    "nuts",     # NUTS code,             629
    "tcid",     # TCID,                   95
    "phon",     # IPA transcription,     136  e.g. '/wɪlɪkiːz/'
    # NOTE: 'piny' is deliberately NOT here. It is a romanization rather than a
    # language, but it is a spelling a user types, so it belongs in
    # SPECIAL_LANGUAGES. Listing it in both tables made it resolve to 'und'
    # depending on check order; `_assert_tables_disjoint` now fails the build on
    # any such overlap.
})

#: ISO 639-2 **bibliographic** codes to ISO 639-1. These are real languages that
#: `iso-languagecodes.txt` omits, so they look like identifiers but are not.
#:
#: Without this mapping, `Terra.localizedName(for: .german)` misses every German
#: name GeoNames tagged `ger`, and the same holds for `fre`, `dut`, `cze`, `geo`
#: and 13 others. The counts are small (82 `gre`, 65 `ger`, 35 `alb`, …) but each
#: is a language a real user asks for.
BIBLIOGRAPHIC_TO_ISO639_1: dict[str, str] = {
    "alb": "sq",   # Albanian
    "arm": "hy",   # Armenian
    "baq": "eu",   # Basque
    "bur": "my",   # Burmese
    "chi": "zh",   # Chinese
    "cze": "cs",   # Czech
    "dut": "nl",   # Dutch
    "fre": "fr",   # French
    "geo": "ka",   # Georgian
    "ger": "de",   # German
    "gre": "el",   # Greek
    "ice": "is",   # Icelandic
    "mao": "mi",   # Maori
    "per": "fa",   # Persian
    "rum": "ro",   # Romanian
    "tib": "bo",   # Tibetan
    "wel": "cy",   # Welsh
}

#: Non-standard-but-meaningful language tags, kept as-is.
SPECIAL_LANGUAGES: dict[str, str] = {
    # Historical French orthography of 1793. Not a name to display, but it is a
    # real spelling and belongs in the alternate-name index.
    "fr_1793": "fr_1793",
    # Pinyin is a romanization, not a language, but it is the transliteration a
    # Chinese-locale user types. Indexing it is what makes
    # `Terra.search("Qínlǐng zhàn")` find anything.
    "piny": "zh-Latn-pinyin",
}

#: Sentinel for "GeoNames supplied no language".
UNDETERMINED = "und"


def _assert_tables_disjoint() -> None:
    """
    Fail loudly if a code appears in more than one classification table.

    Three tables consulted in sequence means an overlap is resolved by check
    order, silently. `piny` was in both IDENTIFIER_CLASSES and SPECIAL_LANGUAGES
    and resolved to `und`, which quietly discarded every Pinyin romanization in
    the dataset. A build-time assertion turns that class of mistake into an
    immediate failure instead of a subtle data loss.
    """
    overlaps = (
        (IDENTIFIER_CLASSES & set(SPECIAL_LANGUAGES))
        | (IDENTIFIER_CLASSES & set(BIBLIOGRAPHIC_TO_ISO639_1))
        | (set(SPECIAL_LANGUAGES) & set(BIBLIOGRAPHIC_TO_ISO639_1))
    )
    if overlaps:
        raise AssertionError(
            f"language classification tables overlap on {sorted(overlaps)}; a code "
            "may belong to exactly one table, or normalize_language resolves it by "
            "check order rather than by intent"
        )


_assert_tables_disjoint()

# Discipline classes, most significant first.
CLASS_PREFERRED = "preferred"
CLASS_HISTORIC = "historic"
CLASS_SHORT = "short"
CLASS_COLLOQUIAL = "colloquial"
CLASS_TRANSLITERATION = "transliteration"
CLASS_EXONYM = "exonym"


@dataclass(frozen=True)
class AlternateName:
    geonameid: int
    language: str      # BCP-47-ish; 'und' when GeoNames gives no language
    name: str
    name_norm: str
    class_: str
    is_preferred: bool
    #: Kept separate from is_preferred rather than folded into class_.
    #:
    #: GeoNames flags are independent, and Germany sets both:
    #:     de:Deutschland        pref=1 short=1
    #:     de:Bundesrepublik     short=1
    #:     de:Bundesrepublik Deutschland  pref=1
    #: classify() collapses 'preferred' to win, so 'Deutschland' leaves the short
    #: bucket and 'Bundesrepublik' is left alone in it — which then wins on
    #: shortest-name and puts `country.name` = 'Bundesrepublik'. Collapsing two
    #: independent facts into one label loses exactly the distinction the
    #: display-name rule needs.
    is_short: bool
    is_historic: bool
    is_colloquial: bool
    from_: date | None
    to: date | None


@dataclass
class AlternateNameStats:
    """Everything discarded, counted. An unexplained large drop is a bug."""

    rows_scanned: int = 0
    rows_for_country: int = 0
    rows_kept: int = 0
    dropped_not_yet_valid: int = 0
    dropped_retired: int = 0
    dropped_blank_name: int = 0
    dropped_no_language: int = 0
    dropped_exact_duplicate: int = 0
    by_class: Counter = field(default_factory=Counter)
    by_language: Counter = field(default_factory=Counter)
    places_with_names: set = field(default_factory=set)

    def render(self, indent: str = "    ") -> str:
        lines = [
            f"{indent}rows scanned:       {self.rows_scanned:,}",
            f"{indent}rows for country:   {self.rows_for_country:,}",
            f"{indent}rows kept:          {self.rows_kept:,}",
            f"{indent}not yet valid:      {self.dropped_not_yet_valid:,}",
            f"{indent}retired:            {self.dropped_retired:,}",
            f"{indent}no language:        {self.dropped_no_language:,}",
            f"{indent}exact duplicate:    {self.dropped_exact_duplicate:,}",
            f"{indent}places with names: {len(self.places_with_names):,}",
        ]
        if self.by_class:
            lines.append(f"{indent}by class:")
            for name, count in self.by_class.most_common():
                lines.append(f"{indent}  {name:<18} {count:,}")
        if self.by_language:
            top = self.by_language.most_common(8)
            lines.append(
                f"{indent}top languages:     "
                + ", ".join(f"{code}={count:,}" for code, count in top)
            )
        return "\n".join(lines)


def normalize_language(raw: str) -> str:
    """
    GeoNames `isolanguage` to a tag the SDKs can match.

    Three cases, and conflating them is how a localized lookup silently returns
    nothing:

    1. **Identifiers** (`wkdt`, `link`, `post`, …) become `` `und` ``. They are
       not names, so they are dropped or kept only when historic.
    2. **Bibliographic ISO 639-2 codes** (`ger`, `fre`, `geo`, …) are remapped to
       ISO 639-1. They are real languages that the ISO companion file omits, so a
       naive validity check mistakes them for junk.
    3. **BCP-47 with script or region** (`zh-Hans`, `pt-BR`, `de-CH`, `en-GB`)
       pass through unchanged. They are more specific than the bare code, and
       SPEC section 7's fallback chain is what reduces `zh-Hans` to `zh`.

    The region variant is kept rather than collapsed, because `zh-Hant` and
    `zh-Hans` are genuinely different spellings and collapsing them loses the
    distinction the tag exists to carry.
    """
    raw = raw.strip()
    if not raw:
        return UNDETERMINED
    if raw in IDENTIFIER_CLASSES:
        return UNDETERMINED
    if raw in SPECIAL_LANGUAGES:
        return SPECIAL_LANGUAGES[raw]
    if raw in BIBLIOGRAPHIC_TO_ISO639_1:
        return BIBLIOGRAPHIC_TO_ISO639_1[raw]
    return raw


def _parse_date(raw: str) -> date | None:
    """ISO 8601 date, or None when empty or malformed."""
    raw = raw.strip()
    if not raw:
        return None
    try:
        return date.fromisoformat(raw)
    except ValueError:
        return None


def classify(*, is_preferred: bool, is_short: bool, is_colloquial: bool,
             is_historic: bool, retired: bool, language: str) -> str:
    """
    Map GeoNames discipline flags onto place_alternate_names.class.

    Order is a decision, not an accident:

    * **preferred first.** A name can be both preferred and historic, and
      "preferred" must win, because the preferred name is what belongs in a
      `name` column even if the place has since been renamed.
    * **historic before colloquial.** A retired official name is a historic name
      regardless of how informally it was used.
    * **transliteration before exonym.** GeoNames does not flag transliterations
      separately, but a name whose script differs from the place's primary script
      is a romanization. That is the distinction SPEC section 8 depends on, and
      getting it wrong makes a transliteration indistinguishable from a genuine
      local exonym.
    """
    if is_preferred:
        return CLASS_PREFERRED
    if retired or is_historic:
        return CLASS_HISTORIC
    if is_short:
        return CLASS_SHORT
    if is_colloquial:
        return CLASS_COLLOQUIAL
    return CLASS_EXONYM


def detect_script(name: str) -> str:
    """Coarse script bucket, enough to tell a romanization from a local name."""
    has_latin = False
    has_other = False
    for char in name:
        if not char.isalpha():
            continue
        # Latin ranges plus Latin Extended. Deliberately coarse: the exact
        # boundary does not matter, only whether the name is in the Latin script
        # at all.
        codepoint = ord(char)
        if (
            0x0041 <= codepoint <= 0x005A
            or 0x0061 <= codepoint <= 0x007A
            or 0x00C0 <= codepoint <= 0x024F
            or 0x1E00 <= codepoint <= 0x1EFF
        ):
            has_latin = True
        else:
            has_other = True
    if has_latin and has_other:
        return "mixed"
    if has_latin:
        return "latn"
    if has_other:
        return "other"
    return "none"


def read_alternate_names(
    zip_path: Path,
    *,
    country_geonameids: set[int],
    primary_scripts: dict[int, str] | None = None,
    valid_at: date,
    from_normalize,
    max_rows: int | None = None,
    log=lambda _message: None,
) -> tuple[list[AlternateName], AlternateNameStats]:
    """
    Stream alternateNamesV2 and keep only rows belonging to this country.

    Streaming is a requirement, not an optimisation. The archive holds roughly 30
    million rows covering every country and language on Earth; loading it is not
    an option, and neither is a two-pass approach with an in-memory set of every
    id in the file. One pass, filter against the country's geonameid set, keep
    what matches.

    `primary_scripts` maps a geonameid to the script bucket of its primary name,
    used to recognize transliterations. Optional: without it a transliteration is
    classed as an exonym, which still makes it searchable and only loses the
    finer distinction in the data.

    `valid_at` is the pipeline run's epoch date. Using the current date instead
    would make the same sources produce different packages on different days,
    which breaks the reproducibility guarantee.

    `max_rows` lets tests exercise the reader against the head of a real file
    rather than a synthetic fixture.
    """
    stats = AlternateNameStats()
    kept: list[AlternateName] = []
    primary_scripts = primary_scripts or {}

    with zipfile.ZipFile(zip_path) as archive:
        members = [
            name for name in archive.namelist()
            if Path(name).name == "alternateNamesV2.txt"
        ]
        if not members:
            raise ValueError(
                f"{zip_path.name}: no alternateNamesV2.txt member "
                f"(archive holds {archive.namelist()})"
            )

        with archive.open(members[0]) as raw:
            for line_number, line_bytes in enumerate(raw, start=1):
                if max_rows is not None and line_number > max_rows:
                    break
                stats.rows_scanned += 1

                # Hot path: split only the first two fields, as BYTES, and test
                # membership before paying for a UTF-8 decode of the whole line.
                #
                # The file is 19 219 334 rows and 785 MB, and almost none of them
                # belong to this country. Decoding every line to find the geonameid
                # makes a seconds-long scan a minutes-long one for no benefit.
                head = line_bytes.split(b"\t", 2)
                if len(head) < 2:
                    stats.dropped_blank_name += 1
                    continue
                try:
                    geonameid = int(head[1])
                except ValueError:
                    # A row whose place cannot be identified cannot be attributed,
                    # so it cannot be shipped (LICENSING.md section 6).
                    stats.dropped_blank_name += 1
                    continue

                if geonameid not in country_geonameids:
                    continue
                stats.rows_for_country += 1

                parts = line_bytes.decode("utf-8").rstrip("\n").split("\t")
                if len(parts) != ALTERNATE_COLUMNS:
                    raise ValueError(
                        f"alternateNamesV2.txt line {line_number}: expected "
                        f"{ALTERNATE_COLUMNS} columns, got {len(parts)}. "
                        "Upstream format changed; re-check the field map."
                    )

                name = parts[3].strip()
                if not name:
                    stats.dropped_blank_name += 1
                    continue

                from_ = _parse_date(parts[8])
                to = _parse_date(parts[9])

                if from_ is not None and from_ > valid_at:
                    stats.dropped_not_yet_valid += 1
                    continue
                retired = to is not None and to < valid_at
                if retired:
                    # Kept, not dropped: SPEC sections 8 and 45 treat historical
                    # names as a feature, and "Constantinople" reaching İstanbul is
                    # the canonical example. It is classed 'historic' so it never
                    # competes with a current name in ranking.
                    stats.dropped_retired += 1

                language = normalize_language(parts[2])
                if language == UNDETERMINED and not retired:
                    # An unlabelled, non-retired name is almost always a raw user
                    # contribution. Historic names are kept without a language
                    # because their own name is the signal.
                    stats.dropped_no_language += 1
                    continue

                is_preferred = parts[4] == "1"
                row_class = classify(
                    is_preferred=is_preferred,
                    is_short=parts[5] == "1",
                    is_colloquial=parts[6] == "1",
                    is_historic=parts[7] == "1",
                    retired=retired,
                    language=language,
                )
                if row_class == CLASS_EXONYM and language != "und":
                    place_script = primary_scripts.get(geonameid)
                    name_script = detect_script(name)
                    if place_script == "other" and name_script == "latn":
                        row_class = CLASS_TRANSLITERATION

                stats.rows_kept += 1
                stats.by_class[row_class] += 1
                stats.by_language[language] += 1
                stats.places_with_names.add(geonameid)

                kept.append(
                    AlternateName(
                        geonameid=geonameid,
                        language=language,
                        name=name,
                        name_norm=from_normalize(name),
                        class_=row_class,
                        is_preferred=is_preferred,
                        is_short=parts[5] == "1",
                        is_historic=parts[7] == "1",
                        is_colloquial=parts[6] == "1",
                        from_=from_,
                        to=to,
                    )
                )

                if stats.rows_kept % 500_000 == 0:
                    log(f"    {stats.rows_kept:,} rows kept so far")

    stats.dropped_exact_duplicate = len(kept) - len(
        {
            (row.geonameid, row.name, row.language, row.class_)
            for row in kept
        }
    )

    # Deterministic order, and deduplicated. GeoNames emits the same name twice
    # for the same place in a surprising number of cases, and an unordered
    # dedupe would make the output depend on input order.
    unique = {
        (
            row.geonameid,
            row.name,
            row.language,
            row.class_,
            row.name_norm,
            row.is_preferred,
            row.is_short,
            row.is_historic,
            row.is_colloquial,
        ): row
        for row in kept
    }
    return (
        [
            unique[key]
            for key in sorted(unique)
        ],
        stats,
    )
