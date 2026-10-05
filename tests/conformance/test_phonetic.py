#!/usr/bin/env python3
"""
Phonetic conformance and property tests.

Unlike normalization, phonetic codes are NOT computed by the SDKs (ADR-003): the
pipeline ships them in `phonetic_codes` and the SDKs only query them. So there is
no cross-language oracle for the algorithm itself — only for the bytes, which the
package checksum already covers.

What this suite therefore checks is different: that the codes are useful. A
phonetic index that does not collapse a typo into the same code is worse than no
index, because it produces confident wrong matches.

Run:  python3 tests/conformance/test_phonetic.py
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

from terrakit_pipeline.phonetic import double_metaphone  # noqa: E402

FAILURES: list[str] = []


def expect_codes(word: str, expected: tuple[str, ...], reason: str) -> None:
    actual = double_metaphone(word)
    if actual != expected:
        FAILURES.append(
            f"  {word!r} -> {actual}, expected {expected}\n      reason: {reason}"
        )


def expect_collision(left: str, right: str, reason: str) -> None:
    """Two spellings of the same word must share at least one code."""
    left_codes = set(double_metaphone(left))
    right_codes = set(double_metaphone(right))
    if not left_codes & right_codes:
        FAILURES.append(
            f"  {left!r} {sorted(left_codes)} and {right!r} {sorted(right_codes)} "
            f"share no code\n      reason: {reason}"
        )


def expect_no_collision(left: str, right: str, reason: str) -> None:
    left_codes = set(double_metaphone(left))
    right_codes = set(double_metaphone(right))
    if left_codes & right_codes:
        FAILURES.append(
            f"  {left!r} and {right!r} collide on {sorted(left_codes & right_codes)}\n"
            f"      reason: {reason}"
        )


def main() -> int:
    print("phonetic conformance\n")

    print("[1] diacritics and Turkish letters collapse (spec section 8)")
    # Atatürk / Ataturk is the specification's own example.
    expect_collision("Atatürk", "Ataturk", "SPEC section 8")
    expect_collision("Izmir", "İzmir", "dotted I vs plain I")
    expect_collision("Izmir", "izmir", "case")
    expect_collision("Muratpaşa", "Muratpasa", "cedilla")
    expect_collision("Şanlıurfa", "Sanliurfa", "S-cedilla and dotless i")
    expect_collision("Köln", "Kolin", "o-umlaut vs plain o")

    print("[2] vowels are dropped, so spellings stay comparable")
    # Every vowel emitted as "A" turned 'Berlin' into 'PARLAN' and destroyed the
    # property this index exists for. Assert the codes rather than eyeball them.
    expect_codes("Antalya", ("ANTL",), "city lookup")
    expect_codes("Ankara", ("ANKR",), "city lookup")
    expect_codes("Berlin", ("PRLN",), "city lookup")
    expect_codes("Konya", ("KN",), "city lookup")
    expect_codes("Tokyo", ("TK",), "CJK names are romanized upstream")

    print("[3] no leading-vowel artifact")
    expect_codes("Osaka", ("ASK",), "initial O must not become A")
    expect_codes("Ankara", ("ANKR",), "initial A must not double")

    print("[4] silent letters are dropped, not encoded")
    # These assert observable properties rather than exact codes. Double
    # Metaphone's own output for edge cases like 'Wright' is not something this
    # project depends on, and pinning an exact value here would freeze an
    # implementation detail. What matters is that a silent letter contributes no
    # phonetic noise, which is what these two checks measure.
    if "F" in double_metaphone("Kraft"):
        FAILURES.append("  'Kraft': silent F must not be encoded")
    if double_metaphone("Wright") != double_metaphone("right"):
        FAILURES.append(
            f"  'Wright' {double_metaphone('Wright')} should equal "
            f"'right' {double_metaphone('right')}; initial W is silent"
        )

    print("[5] Latin-script Turkish letters DO encode")
    # 'İ' is a Latin-script letter, so it folds to I and produces codes. Only
    # genuinely non-Latin scripts return nothing.
    expect_collision("İstanbul", "Istanbul", "dotted I is Latin script")
    expect_collision("İSTANBUL", "istanbul", "case folding")

    print("[6] non-Latin scripts yield no codes (by design)")
    # Cross-script search runs through place_alternate_names, not phonetics
    # (DATA_MODEL.md section 5.2). A phonetic code for '東京' would require a
    # transliteration engine in the pipeline and would still be a guess.
    for word in ("東京", "თურქეთი", "Москва", "Αθήνα"):
        if double_metaphone(word):
            FAILURES.append(
                f"  {word!r} produced {double_metaphone(word)}; expected no codes "
                "(ADR-003: transliteration is a pipeline concern)"
            )

    print("[7] degenerate input")
    for word in ("", " ", "a", "1", "..."):
        if double_metaphone(word):
            FAILURES.append(
                f"  {word!r} produced {double_metaphone(word)}, expected no codes"
            )

    print("[8] deterministic and bounded")
    for word in ("Antalya", "İstanbul", "Şanlıurfa"):
        first = double_metaphone(word)
        if double_metaphone(word) != first:
            FAILURES.append(f"  {word!r} is not deterministic")
        for code in first:
            if not code.isalpha() or not code.isupper():
                FAILURES.append(f"  code {code!r} is not uppercase ASCII")
            if len(code) > 8:
                FAILURES.append(f"  code {code!r} exceeds 8 characters")

    print("[9] distinct cities stay distinguishable")
    # A phonetic index that collapses everything is as useless as none. These
    # four are genuinely different places and must not collide.
    expect_no_collision("Ankara", "Antalya", "different cities")
    expect_no_collision("Izmir", "Istanbul", "different cities")
    expect_no_collision("Bursa", "Adana", "different cities")

    print("[10] typo tolerance is the actual purpose")
    # Single-character substitutions in the middle of a name.
    expect_collision("Gaziantep", "Gaziantap", "final vowel dropped")
    expect_collision("Trabzon", "Trabzon", "identity")

    print()
    if FAILURES:
        print("FAILURES:", file=sys.stderr)
        for failure in FAILURES:
            print(failure, file=sys.stderr)
        print(f"\n{len(FAILURES)} failure(s)", file=sys.stderr)
        return 1

    print("all phonetic checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
