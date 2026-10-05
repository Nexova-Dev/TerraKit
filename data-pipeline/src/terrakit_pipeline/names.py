"""
Name attachment — localized names, exonyms and historic names onto places.

Separated from hierarchy.py because it is a different concern with a different
failure mode. Hierarchy construction gets the *structure* right or the package is
wrong in an obvious way. Names can be subtly, invisibly wrong — a Wikipedia URL
in a name column, "Bundesrepublik Deutschland" as a country's display name, a
retired 1930 spelling competing with the current one — and nothing errors.

This module holds the two decisions that matter:

**Which name is the place's display name.** Measured across all six MVP countries,
GeoNames' `isPreferredName` is the *formal* state name, not the everyday one:

    TR   pref=1  tr:Türkiye Cumhuriyeti        short=1  tr:Türkiye
    DE   pref=1  de:Bundesrepublik Deutschland short=1  de:Deutschland
    GE   pref=1  ka:საქართველო             short=0
    JP   pref=1  ja:日本                       short=0
    GB   pref=1  en:United Kingdom            short=1  en:United Kingdom

So preferring the `pref` flag puts "Bundesrepublik Deutschland" in
`country.name`, which no developer would write. The rule is therefore:

    1. a short name in the country's own language  ->  Türkiye, Deutschland
    2. else a preferred name in that language      ->  საქართველო, 日本
    3. else the name the upstream source gives    ->  never empty

The formal variant is not discarded — it stays searchable as an alternate name,
which is where it belongs.

**Which names are searchable.** Everything GeoNames publishes for a place, minus
the ones that are not names at all (Wikidata Q-ids, Wikipedia URLs, UN/LOCODE).
That filtering happens in alternate_names.py; this module only attaches.
"""

from __future__ import annotations

from .alternate_names import (
    CLASS_EXONYM,
    CLASS_HISTORIC,
    CLASS_PREFERRED,
    CLASS_SHORT,
    CLASS_TRANSLITERATION,
    UNDETERMINED,
    AlternateName,
    detect_script,
)
from .hierarchy import AlternateNameRow, Place, PlaceName

__all__ = ["attach_names", "resolve_display_name", "primary_language_of"]

#: A name must be at least this long to be worth indexing as a search key.
#: Below it, the trigram tokenizer cannot match it (DATA_MODEL.md section 9), so
#: the row would cost bytes and be unreachable.
MIN_SEARCHABLE_LENGTH = 3

#: Cap per (place, language) in place_alternate_names. GeoNames publishes up to a
#: few hundred names in one language for a large city; they are sorted by a
#: quality ranking and the tail is noise for an address autocomplete.
MAX_ALTERNATES_PER_LANGUAGE = 12


def primary_language_of(languages_field: str) -> str | None:
    """
    The country's own language tag, from countryInfo's `languages` column.

    GeoNames lists them in priority order, so the first entry is the primary one:
    TR gives 'tr-TR,ku,diq,az,av' -> 'tr', GB gives 'en-GB,cy-GB,gd' -> 'en'.
    The region subtag is stripped because alternate names are tagged with the bare
    language, and 'en-GB' would not match a row tagged 'en'.
    """
    first = languages_field.split(",", 1)[0].strip() if languages_field else ""
    if not first:
        return None
    return first.split("-", 1)[0].strip().lower() or None


def _quality_rank(row: AlternateName) -> tuple:
    """
    Ordering for choosing among competing names in one language.

    Preferred first, then shorter (a short name is the everyday one, per the
    measurement above), then alphabetical so the result does not depend on input
    order — which matters because a non-deterministic choice would break the
    reproducibility guarantee.
    """
    return (
        0 if row.class_ == CLASS_PREFERRED else 1,
        len(row.name),
        row.name,
    )


def _display_name_for_country(rows: list[AlternateName]) -> AlternateName:
    """
    A country's own name for itself, measured across all six MVP countries.

        TR   tr:Türkiye Cumhuriyeti  pref=1    tr:Türkiye       short=1
        DE   de:Bundesrepublik …     pref=1    de:Deutschland   short=1
        GE   ka:საქართველო        pref=1    (no short form)
        JP   ja:日本                   pref=1    (no short form)
        GB   en:United Kingdom        pref=1    en:United Kingdom short=1
        US   en:United States         pref=1    en:United States  short=1

    `isPreferredName` is the *formal* name — the legal or long form. Picking it
    puts 'Bundesrepublik Deutschland' in `country.name`, which no developer would
    write and no user would search for.

    Rule: shortest short form, else shortest preferred name. Where a single row
    carries both flags (Deutschland, United Kingdom) the choice is the same either
    way.

    "Shortest within the class", not "first in the class": Germany publishes
    `de:Bundesrepublik` (short) *before* `de:Deutschland` (short and preferred), so
    taking the first short row yields 'Bundesrepublik'. Both branches therefore
    rank by (length, name), which is also what makes the result independent of the
    order upstream happens to emit rows in — a reproducibility requirement, not a
    preference.
    """
    # Uses the raw is_short flag, not class_ == CLASS_SHORT: classify() lets
    # 'preferred' win when both are set, so 'Deutschland' (pref=1 short=1) is not
    # in the short bucket while 'Bundesrepublik' (short=1) is. Reading the flag
    # directly is what puts Deutschland back in contention.
    shorts = [row for row in rows if row.is_short]
    if shorts:
        return min(shorts, key=lambda row: (len(row.name), row.name))
    preferred = [row for row in rows if row.class_ == CLASS_PREFERRED]
    if preferred:
        return min(preferred, key=lambda row: (len(row.name), row.name))
    return min(rows, key=lambda row: (len(row.name), row.name))


def attach_names(
    places: list[Place],
    alternate_rows: list[AlternateName],
    *,
    country_language: str | None,
    from_normalize,
    country_geonameid: int | None = None,
) -> tuple[list[PlaceName], list[AlternateNameRow], dict[str, int]]:
    """
    Build place_names and place_alternate_names for a package.

    Returns (place_names, alternate_names, stats).
    """
    # geonameid -> place id. The join key, since upstream rows carry geonameid and
    # the package carries namespaced ids (ADR-002).
    by_source_row: dict[str, Place] = {
        place.source_row_id: place for place in places if place.source_row_id
    }
    if country_geonameid is not None:
        country_place = next(
            (p for p in places if p.source_row_id == str(country_geonameid)), None
        )
        if country_place is not None:
            by_source_row.setdefault(str(country_geonameid), country_place)

    grouped: dict[tuple[str, str], list[AlternateName]] = {}
    place_by_id = {place.id: place for place in places}
    for row in alternate_rows:
        place = by_source_row.get(str(row.geonameid))
        if place is None:
            # The name belongs to a place this package does not contain, e.g. a
            # settlement below the cities500 population threshold. Not an error.
            continue
        grouped.setdefault((place.id, row.language), []).append(row)

    place_names: list[PlaceName] = []
    alternate_names: list[AlternateNameRow] = []
    seen_place_names: set[tuple[str, str]] = set()
    promoted_display_names: list[str] = []

    for (place_id, language), rows in sorted(grouped.items()):
        place = place_by_id[place_id]
        rows.sort(key=_quality_rank)

        # --- place_names: one authoritative name per language ---------------
        # Prefer an explicit preferred row; otherwise the shortest, which by the
        # measurement above is the everyday form.
        chosen = next(
            (r for r in rows if r.class_ == CLASS_PREFERRED),
            rows[0],
        )
        if (place_id, language) not in seen_place_names:
            seen_place_names.add((place_id, language))
            place_names.append(
                PlaceName(
                    place_id=place_id,
                    lang=language,
                    name=chosen.name,
                    name_norm=from_normalize(chosen.name),
                    script=detect_script(chosen.name),
                    is_preferred=(chosen.class_ == CLASS_PREFERRED),
                    is_short=(chosen.class_ == CLASS_SHORT),
                    source="geonames_alternatenames",
                )
            )

        # --- place_alternate_names: everything else, searchable ------------
        kept = 0
        for row in rows:
            if row.name == chosen.name and row.language == language:
                # Already authoritative; indexing it twice would double-count it
                # in search ranking.
                continue
            if len(from_normalize(row.name)) < MIN_SEARCHABLE_LENGTH:
                continue
            if kept >= MAX_ALTERNATES_PER_LANGUAGE:
                break
            kept += 1
            alternate_names.append(
                AlternateNameRow(
                    place_id=place_id,
                    name=row.name,
                    name_norm=from_normalize(row.name),
                    lang=None if language == UNDETERMINED else language,
                    class_=row.class_,
                    source="geonames_alternatenames",
                )
            )

    # --- display names ------------------------------------------------------
    #
    # ONLY the country row is promoted, and the restriction is deliberate.
    #
    # countryInfo.txt publishes the country name in English — 'Turkey', 'Georgia'
    # — so `country.name` is wrong for a Turkish or Georgian application, and SPEC
    # section 7's fallback chain ends at the local name. That is a real defect
    # worth fixing.
    #
    # Every other level already carries a local name from the source of record:
    # cities500.txt `name` is the local settlement name, and admin1CodesASCII.txt
    # publishes both the native and the ASCII form. Promoting a preferred name
    # there would replace 'Antalya Province' with 'Antalya' and drop the word that
    # distinguishes a province from a city — a worse outcome than leaving
    # GeoNames' own choice alone.
    #
    # So: one place, one rule, one known-bad case fixed, nothing else touched.
    country_place = next(
        (place for place in places if place.level == 1 and place.parent_id is None),
        None,
    )
    if country_place is not None and country_language:
        own_rows = grouped.get((country_place.id, country_language), [])
        if own_rows:
            display = _display_name_for_country(own_rows)
            if display.name_norm != country_place.name_norm:
                promoted_display_names.append(
                    f"{country_place.id}: {country_place.name!r} -> {display.name!r}"
                )
                country_place.name = display.name
                country_place.name_norm = display.name_norm

                # place_names must agree with the display name, or
                # `localizedName(for: .turkish)` returns 'Türkiye Cumhuriyeti'
                # while `country.name` returns 'Türkiye' — two different answers
                # to the same question in the same package.
                # Rebuilt as a PlaceName rather than reusing the AlternateName:
                # `display` is the upstream row, and dropping it into place_names
                # makes every later consumer of that list read a field that does
                # not exist on the type it expects.
                promoted_entry = PlaceName(
                    place_id=country_place.id,
                    lang=country_language,
                    name=display.name,
                    name_norm=display.name_norm,
                    script=detect_script(display.name),
                    is_preferred=(display.class_ == CLASS_PREFERRED),
                    is_short=(display.class_ == CLASS_SHORT),
                    source="geonames_alternatenames",
                )
                for index, existing in enumerate(place_names):
                    if (
                        existing.place_id == country_place.id
                        and existing.lang == country_language
                    ):
                        place_names[index] = promoted_entry
                        break
                else:
                    place_names.append(promoted_entry)

    stats = {
        "place_names": len(place_names),
        "alternate_names": len(alternate_names),
        "languages": len({name.lang for name in place_names}),
        "places_with_names": len({name.place_id for name in place_names}),
        "historic_names": sum(
            1 for row in alternate_names if row.class_ == CLASS_HISTORIC
        ),
        "transliterations": sum(
            1 for row in alternate_names if row.class_ == CLASS_TRANSLITERATION
        ),
        "exonyms": sum(
            1 for row in alternate_names if row.class_ == CLASS_EXONYM
        ),
        "display_names_promoted": len(promoted_display_names),
    }
    return place_names, alternate_names, stats
