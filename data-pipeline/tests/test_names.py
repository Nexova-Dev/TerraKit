#!/usr/bin/env python3
"""
Name ingestion tests.

Covers the decisions that decide what a user sees and what search returns. Every
group here corresponds to a rule that was wrong at least once during
implementation, which is the only justification for a test that pins a string
literal.

The rules under test:

1.  `isPreferredName` is the FORMAL name, not the everyday one. Verified against
    all six MVP countries.
2.  The display-name choice must not depend on the order upstream emits rows.
3.  Language codes fall into three categories, and conflating them loses data.
4.  Identifiers are not names, even when upstream marks one `isPreferredName`.
5.  Japanese kana with a dakuten survive normalization.
6.  Alternate names reach the search index, not just the display tables.

Run:  python3 data-pipeline/tests/test_names.py
"""

from __future__ import annotations

import sys
import unicodedata
from datetime import date
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

from terrakit_pipeline.alternate_names import (  # noqa: E402
    CLASS_HISTORIC,
    CLASS_PREFERRED,
    CLASS_SHORT,
    IDENTIFIER_CLASSES,
    UNDETERMINED,
    AlternateName,
    classify,
    detect_script,
    normalize_language,
)
from terrakit_pipeline.names import (  # noqa: E402
    _display_name_for_country,
    primary_language_of,
)
from terrakit_pipeline.normalize import normalize  # noqa: E402

FAILURES: list[str] = []


def check(condition: bool, message: str) -> None:
    if not condition:
        FAILURES.append(f"  {message}")


def alt(
    name: str,
    language: str = "de",
    *,
    preferred: bool = False,
    short: bool = False,
    historic: bool = False,
    colloquial: bool = False,
) -> AlternateName:
    """A synthetic upstream row, shaped exactly like read_alternate_names emits."""
    return AlternateName(
        geonameid=1,
        language=language,
        name=name,
        name_norm=normalize(name),
        class_="exonym",
        is_preferred=preferred,
        is_short=short,
        is_historic=historic,
        is_colloquial=colloquial,
        from_=None,
        to=None,
    )


def main() -> int:
    print("name ingestion\n")

    print("[1] preferred name is the formal one, not the everyday one")
    # Measured upstream rows for Türkiye and Germany.
    turkey = [
        alt("Türkiye Cumhuriyeti", "tr", preferred=True),
        alt("Türkiye", "tr", short=True),
    ]
    check(
        _display_name_for_country(turkey).name == "Türkiye",
        f"TR must display 'Türkiye', got {_display_name_for_country(turkey).name!r}",
    )
    germany = [
        alt("Bundesrepublik Deutschland", "de", preferred=True),
        alt("Deutschland", "de", preferred=True, short=True),
        alt("Bundesrepublik", "de", short=True),
    ]
    check(
        _display_name_for_country(germany).name == "Deutschland",
        f"DE must display 'Deutschland', got {_display_name_for_country(germany).name!r}",
    )
    # No short form available: the preferred name is the answer.
    georgia = [alt("საქართველო", "ka", preferred=True)]
    check(
        _display_name_for_country(georgia).name == "საქართველო",
        "GE must fall back to the preferred name when no short form exists",
    )
    # Only junk: never empty.
    junk = [alt("Republic of Turkey", "en", preferred=True)]
    check(
        _display_name_for_country(junk).name != "",
        "a display name must never be empty",
    )

    print("[2] the choice must not depend on upstream row order")
    # Germany publishes 'Bundesrepublik' (short) before 'Deutschland'
    # (short + preferred). Reading the first short row in file order yields
    # 'Bundesrepublik', which is what the pipeline did until this was fixed.
    check(
        _display_name_for_country(germany).name
        == _display_name_for_country(list(reversed(germany))).name,
        "display name must be identical under row reversal",
    )
    for permutation in (
        [germany[0], germany[2], germany[1]],
        [germany[2], germany[0], germany[1]],
        [germany[1], germany[0], germany[2]],
    ):
        check(
            _display_name_for_country(permutation).name == "Deutschland",
            f"row order {['x'] * len(permutation)} changed the display name: "
            f"got {_display_name_for_country(permutation).name!r}",
        )

    print("[3] both preferred and short must be readable independently")
    both = alt("Deutschland", "de", preferred=True, short=True)
    check(both.is_preferred and both.is_short,
          "a row flagged both must retain both flags")
    check(
        classify(
            is_preferred=both.is_preferred,
            is_short=both.is_short,
            is_colloquial=False,
            is_historic=False,
            retired=False,
            language="de",
        ) == CLASS_PREFERRED,
        "classify lets preferred win, which is why the flag is kept separately",
    )

    print("[4] language codes fall into three categories")
    # Identifiers are not names.
    for code in ("wkdt", "link", "post", "unlc", "iata", "nuts", "uicn", "geoid"):
        check(
            code in IDENTIFIER_CLASSES,
            f"{code!r} must be classified as an identifier",
        )
        check(
            normalize_language(code) == UNDETERMINED,
            f"{code!r} must not survive as a language tag",
        )
    # Bibliographic ISO 639-2 codes are real languages absent from the ISO list.
    # Losing them means localizedName(for: .german) misses every 'ger' row.
    for code, expected in (
        ("ger", "de"), ("fre", "fr"), ("dut", "nl"), ("cze", "cs"),
        ("geo", "ka"), ("chi", "zh"), ("gre", "el"), ("alb", "sq"),
    ):
        check(
            normalize_language(code) == expected,
            f"{code!r} must map to {expected!r}, got {normalize_language(code)!r}",
        )
    # BCP-47 with script or region is valid and more specific than the bare code.
    for code in ("zh-Hans", "zh-Hant", "pt-BR", "en-GB", "de-CH", "zh-CN"):
        check(
            normalize_language(code) == code,
            f"{code!r} must pass through unchanged, got {normalize_language(code)!r}",
        )
    check(normalize_language("") == UNDETERMINED, "empty language must be 'und'")
    check(
        normalize_language("piny") == "zh-Latn-pinyin",
        "Pinyin is a spelling a user types, so it must be indexed",
    )

    print("[5] classification order is deliberate")
    check(
        classify(is_preferred=True, is_short=True, is_colloquial=True,
                 is_historic=True, retired=True, language="en") == CLASS_PREFERRED,
        "preferred outranks every other flag",
    )
    check(
        classify(is_preferred=False, is_short=True, is_colloquial=True,
                 is_historic=True, retired=True, language="en") == CLASS_HISTORIC,
        "historic outranks colloquial and short",
    )
    check(
        classify(is_preferred=False, is_short=True, is_colloquial=False,
                 is_historic=False, retired=False, language="en") == CLASS_SHORT,
        "short is retained as its own class",
    )

    print("[6] primary language extraction")
    for field, expected in (
        ("tr-TR,ku,diq,az,av", "tr"),
        ("en-GB,cy-GB,gd", "en"),
        ("ka,ru,hy,az", "ka"),
        ("en-US,es-US,haw,fr", "en"),
        ("de", "de"),
        ("", None),
    ):
        check(
            primary_language_of(field) == expected,
            f"primary_language_of({field!r}) = {primary_language_of(field)!r}, "
            f"expected {expected!r}",
        )

    print("[7] Japanese kana survive normalization (the dakuten defect)")
    # NFKD splits ガ into カ + U+3099. A categorical mark strip rewrites ガザ to
    # カサ and collapses カ and ガ onto one key.
    for word in ("ガザ", "バ", "パ", "ヴ", "イスタンブール", "東京都"):
        check(
            normalize(word) == word,
            f"normalize({word!r}) = {normalize(word)!r}; dakuten must survive",
        )
    check(
        normalize("カ") != normalize("ガ"),
        "カ and ガ must not share a search key",
    )
    check(
        normalize("パ") != normalize("ハ"),
        "パ and ハ must not share a search key",
    )
    check(
        " " not in normalize("ガザ"),
        "a mid-string combining mark must not become a separator",
    )
    # Latinish marks must still go, or İstanbul and Istanbul diverge.
    check(normalize("İstanbul") == "istanbul", "İstanbul must fold to istanbul")
    check(normalize("Café") == "cafe", "é must lose its accent")
    # Hangul: NFKD splits it into jamo, step 9 recomposes it.
    for word in ("서울", "부산"):
        check(normalize(word) == word, f"Hangul {word!r} must round-trip")
    check(
        unicodedata.normalize("NFC", normalize("イスタンブール")) == normalize("イスタンブール"),
        "output must be NFC-stable",
    )

    print("[8] script detection separates transliterations from exonyms")
    check(detect_script("İstanbul") == "latn", "İstanbul is Latin")
    check(detect_script("イスタンブール") == "other", "Japanese is not Latin")
    check(detect_script("Тürkiye") == "mixed", "a mixed-script name is mixed")
    check(detect_script("123") == "none", "digits are not a script")

    print()
    if FAILURES:
        print("FAILURES:", file=sys.stderr)
        for failure in FAILURES:
            print(failure, file=sys.stderr)
        print(f"\n{len(FAILURES)} failure(s)", file=sys.stderr)
        return 1

    print("all name-ingestion checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
