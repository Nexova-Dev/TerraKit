#!/usr/bin/env python3
"""
Reference implementation of the TerraKit normalization spec.

This is the ORACLE. Each SDK implements the same algorithm independently
(core/spec/normalization.md), and CI compares all of them against the fixture
in tests/fixtures/normalization.json.

If this implementation and an SDK disagree, one of them is wrong. Never edit
the fixture to resolve a disagreement — see CONTRIBUTING.md section 5.1.

Usage:
    python3 tests/conformance/run_normalization.py [--verbose]

Exit codes:
    0  all vectors pass
    1  a vector failed
    2  fixture missing or malformed
"""

from __future__ import annotations

import json
import sys
import unicodedata
from pathlib import Path

# --- core/spec/normalization.md section 3 ---------------------------------
# Applied after NFKD + mark stripping, BEFORE case folding.
SPECIAL_CASES: dict[str, str] = {
    "ß": "ss",   # ß LATIN SMALL LETTER SHARP S
    "ẞ": "ss",   # ẞ LATIN CAPITAL LETTER SHARP S
    "æ": "ae", "ǽ": "ae",
    "œ": "oe", "Œ": "oe",
    "ø": "o", "Ø": "o",
    "đ": "d", "Đ": "d",
    "ð": "d", "Ð": "d",
    "þ": "th", "Þ": "th",
    "ł": "l", "Ł": "l",
    "ı": "i",            # Turkish dotless i
    "İ": "i",            # Turkish dotted I
    "ħ": "h", "Ħ": "h",
    "ŋ": "ng", "Ŋ": "ng",
    "ŧ": "t", "Ŧ": "t",
    "ə": "e",
    "­": "",       # soft hyphen U+00AD, removed
    "‐": "-", "‑": "-", "‒": "-", "–": "-",
    "—": "-", "―": "-", "−": "-",
    # Apostrophes and quotation marks are REMOVED, not turned into spaces
    # (spec section 6): D'Angelo -> dangelo, not "d angelo".
    "'": "", "’": "", "‘": "", "ʼ": "", "`": "",
    " ": " ",       # no-break space
}

MARK_CATEGORIES = frozenset(("Mn", "Mc", "Me"))


def normalize(input: str) -> str:
    """core/spec/normalization.md — normative algorithm."""
    # 1. NFKD decomposition
    text = unicodedata.normalize("NFKD", input)

    # 2. Strip combining marks (Mn, Mc, Me)
    text = "".join(c for c in text if unicodedata.category(c) not in MARK_CATEGORIES)

    # 3. Special-case substitutions
    text = "".join(SPECIAL_CASES.get(c, c) for c in text)

    # 4. Full Unicode case folding.
    #    str.casefold() implements CaseFolding.txt F + C mappings.
    #    T (Turkic) mappings are NOT applied — see spec section 4.
    text = text.casefold()

    # 5. Transliteration is pipeline-only. SDKs do not transliterate.
    #    (Python's casefold would mangle non-Latin, but spec vectors 29/30
    #    expect CJK and Georgian to pass through unchanged, and the pipeline
    #    transliteration step is applied to source data, not to SDK queries.)

    # 6-8. Separator handling: runs of non-letter/non-number -> single space,
    #      then trim, then collapse internal runs.
    out: list[str] = []
    pending_space = False
    for ch in text:
        if unicodedata.category(ch)[0] in ("L", "N"):
            if pending_space and out:
                out.append(" ")
            out.append(ch)
            pending_space = False
        else:
            pending_space = True
    return "".join(out)


def postal_normalize(code: str) -> str:
    """
    Stricter postal-code normalization, DATA_MODEL.md section 8.

    Letters and digits only. No separator collapsing, so 'SW1A 1AA' and
    'SW1A1AA' both reach 'sw1a1aa'. Leading zeros are preserved.
    """
    text = unicodedata.normalize("NFKC", code)
    return "".join(c for c in text if c.isalnum()).casefold()


def main() -> int:
    verbose = "--verbose" in sys.argv or "-v" in sys.argv
    fixture_path = (
        Path(__file__).resolve().parents[1] / "fixtures" / "normalization.json"
    )
    if not fixture_path.exists():
        print(f"error: fixture not found: {fixture_path}", file=sys.stderr)
        return 2

    try:
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        print(f"error: fixture is malformed: {exc}", file=sys.stderr)
        return 2

    vectors = fixture.get("vectors", [])
    if not vectors:
        print("error: fixture contains no vectors", file=sys.stderr)
        return 2

    failures: list[str] = []
    for vector in vectors:
        n = vector["n"]
        raw = vector["input"]
        expected = vector["expected"]
        actual = normalize(raw)

        if actual != expected:
            failures.append(
                f"  #{n:>2} {vector['covers']}\n"
                f"      input:    {raw!r}\n"
                f"      expected: {expected!r}\n"
                f"      actual:   {actual!r}"
            )
        elif verbose:
            print(f"  ok  #{n:>2} {raw!r} -> {actual!r}")

    total = len(vectors)
    passed = total - len(failures)

    print(f"normalization conformance: {passed}/{total} vectors passed")

    if failures:
        print("\nFAILURES:", file=sys.stderr)
        for failure in failures:
            print(failure, file=sys.stderr)
        print(
            "\nDo not edit the fixture to make these pass. "
            "See CONTRIBUTING.md section 5.1.",
            file=sys.stderr,
        )
        return 1

    # Invariant checks, per spec section 8.
    for vector in vectors:
        result = normalize(vector["input"])
        if normalize(result) != result:
            failures.append(f"  #{vector['n']} not idempotent")
        if any(unicodedata.category(c) in MARK_CATEGORIES for c in result):
            failures.append(f"  #{vector['n']} output contains combining marks")
        if any(unicodedata.category(c) == "Cc" for c in result):
            failures.append(f"  #{vector['n']} output contains control characters")

    if failures:
        print("\nINVARIANT FAILURES:", file=sys.stderr)
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1

    print("invariants: idempotence, no combining marks, no control characters — OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
