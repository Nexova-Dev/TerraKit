#!/usr/bin/env python3
"""
Normalization conformance runner.

This is the ORACLE. The Swift, Kotlin and Rust implementations are checked
against it via tests/fixtures/normalization.json.

The algorithm itself lives in
data-pipeline/src/terrakit_pipeline/normalize.py — deliberately NOT copied
here. A second Python copy would drift, and normalization drift means the SDKs
return different results for the same query string, which is exactly what the
conformance suite exists to prevent.

If this and an SDK disagree, one of them is wrong. Never edit the fixture to
resolve a disagreement. See CONTRIBUTING.md section 5.1.

Usage:
    python3 tests/conformance/run_normalization.py [--verbose]
    python3 tests/conformance/run_normalization.py --print-impls
"""

from __future__ import annotations

import argparse
import json
import sys
import unicodedata
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

from terrakit_pipeline.normalize import (  # noqa: E402
    _LATINISH_SCRIPTS,
    MARK_CATEGORIES,
    normalize,
    postal_normalize,
    search_key_unaccented,
)

FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "normalization.json"


def check_postal_normalization() -> list[str]:
    """
    DATA_MODEL.md section 8: postal codes use a stricter normalizer.

    These assertions live here rather than in the fixture because
    postal_normalize is a second public function of the pipeline, not an SDK
    concern. SDKs receive the indexed code_norm value and never normalize a
    postal code at query time.
    """
    cases = [
        ("SW1A 1AA", "sw1a1aa", "GB spaced and compact forms must collide"),
        ("SW1A1AA", "sw1a1aa", "GB compact"),
        ("SW1A-1AA", "sw1a1aa", "GB hyphenated"),
        ("07160", "07160", "TR leading zero preserved"),
        ("07000", "07000", "TR leading zero preserved"),
        ("10115", "10115", "DE"),
        ("  07000  ", "07000", "TR whitespace trimmed"),
        ("sw1a 1aa", "sw1a1aa", "GB lowercase input"),
    ]
    failures = []
    for raw, expected, reason in cases:
        actual = postal_normalize(raw)
        if actual != expected:
            failures.append(
                f"  postal_normalize({raw!r}) = {actual!r}, expected {expected!r}\n"
                f"      reason: {reason}"
            )
    return failures


def check_search_key() -> list[str]:
    """
    The second trigram column strips separators entirely, so a single-token
    query like 'AtatürkCaddesi' can match 'Atatürk Caddesi'.
    """
    cases = [
        ("Atatürk Caddesi", "ataturkcaddesi", "spaces removed"),
        ("Ataturk Caddesi", "ataturkcaddesi", "unaccented equivalent"),
        ("New York", "newyork", "ascii"),
        ("東京都", "東京都", "CJK unchanged"),
        ("SW1A 1AA", "sw1a1aa", "postal-like input"),
    ]
    failures = []
    for raw, expected, reason in cases:
        actual = search_key_unaccented(raw)
        if actual != expected:
            failures.append(
                f"  search_key_unaccented({raw!r}) = {actual!r}, "
                f"expected {expected!r}\n      reason: {reason}"
            )
    return failures


def check_latinish_marks_stripped() -> list[str]:
    """
    No combining mark may survive on a Latin, Greek or Cyrillic base.

    Replaces the old blanket "no combining marks in the output" invariant, which
    was wrong rather than merely incomplete. NFKD decomposes Japanese kana with a
    dakuten into a base kana plus an Mn mark, so requiring zero marks would have
    rejected the correct output for ガザ and forced implementations to destroy
    Japanese place names. The invariant that actually matters is narrower: marks
    on Latinish bases are decoration and must go; marks elsewhere are the
    difference between two distinct kana and must stay.
    """
    import unicodedata

    failures: list[str] = []
    for vector in load_vectors():
        result = normalize(vector["input"])
        base_is_latinish = False
        for char in result:
            if unicodedata.category(char) in MARK_CATEGORIES:
                if base_is_latinish:
                    failures.append(
                        f"  #{vector['n']} kept a combining mark on a Latinish "
                        f"base: {result!r}"
                    )
                    break
                continue
            try:
                script = unicodedata.name(char).split(" ", 1)[0]
            except ValueError:
                script = ""
            base_is_latinish = script in _LATINISH_SCRIPTS
    return failures


def load_vectors() -> list[dict]:
    fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
    return fixture.get("vectors", [])


def check_fuzz_idempotence() -> list[str]:
    """
    Seeded fuzz over mixed scripts.

    Catches divergences in codepoints nobody thought to put in a hand-written
    vector. Deterministic: same seed on every machine, so a failure reproduces.
    """
    import random

    rng = random.Random(20261005)
    alphabet = (
        "abcdeıİiIßẞøØłŁğüşçöĞÜŞÇÖ"
        "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ"
        "αβγδεζηθικλμνξοπρστυφχψω"
        "აბგდევზთიკლმნოპჟრსტუფქღყშჩცძწჭხჯჰ"
        "あいうえお東京İstanbul-"
    )
    failures = []
    for i in range(2000):
        raw = "".join(rng.choice(alphabet) for _ in range(rng.randint(1, 24)))
        once = normalize(raw)
        twice = normalize(once)
        if once != twice:
            failures.append(
                f"  not idempotent: {raw!r}\n"
                f"      once:  {once!r}\n"
                f"      twice: {twice!r}"
            )
            if len(failures) > 10:
                break
            continue
        base_is_latinish = False
        for c in once:
            if unicodedata.category(c) in MARK_CATEGORIES:
                if base_is_latinish:
                    failures.append(
                        f"  Latinish combining mark survived: {raw!r} -> {once!r}"
                    )
                    break
                continue
            try:
                script = unicodedata.name(c).split(" ", 1)[0]
            except ValueError:
                script = ""
            base_is_latinish = script in _LATINISH_SCRIPTS
        if failures and len(failures) > 10:
            break
        if any(unicodedata.category(c) == "Cc" for c in once):
            failures.append(f"  control character survived: {raw!r} -> {once!r}")
            if len(failures) > 10:
                break
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verbose", "-v", action="store_true")
    parser.add_argument(
        "--print-impls",
        action="store_true",
        help="print the reference implementations for porting to Swift/Kotlin/Rust",
    )
    args = parser.parse_args()

    if args.print_impls:
        from terrakit_pipeline import normalize as mod

        print(f"SPECIAL_CASES ({len(mod.SPECIAL_CASES)} entries):")
        for key, value in mod.SPECIAL_CASES.items():
            print(f"  U+{ord(key):04X} {key!r} -> {value!r}")
        print(f"\nMARK_CATEGORIES = {sorted(MARK_CATEGORIES)}")
        return 0

    if not FIXTURE_PATH.exists():
        print(f"error: fixture not found: {FIXTURE_PATH}", file=sys.stderr)
        return 2

    try:
        fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
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
                f"  #{n} {vector.get('covers', '')}\n"
                f"      input:    {raw!r}\n"
                f"      expected: {expected!r}\n"
                f"      actual:   {actual!r}"
            )
        elif args.verbose:
            print(f"  ok  #{n:>2} {raw!r} -> {actual!r}")

    passed = len(vectors) - len(
        [f for f in failures if f.strip().startswith("#")]
    )
    print(f"normalization conformance: {passed}/{len(vectors)} vectors passed")

    # Spec section 8 invariants.
    invariant_failures = []
    for vector in vectors:
        n = vector["n"]
        result = normalize(vector["input"])
        if normalize(result) != result:
            invariant_failures.append(f"  #{n} not idempotent: {result!r}")
        base_is_latinish = False
        for c in result:
            if unicodedata.category(c) in MARK_CATEGORIES:
                if base_is_latinish:
                    invariant_failures.append(
                        f"  #{n} kept a combining mark on a Latinish base"
                    )
                    break
                continue
            try:
                script = unicodedata.name(c).split(" ", 1)[0]
            except ValueError:
                script = ""
            base_is_latinish = script in _LATINISH_SCRIPTS
        if any(unicodedata.category(c) == "Cc" for c in result):
            invariant_failures.append(f"  #{n} output contains control characters")

    # Pipeline-only functions.
    latinish_failures = check_latinish_marks_stripped()
    postal_failures = check_postal_normalization()
    search_key_failures = check_search_key()
    fuzz_failures = check_fuzz_idempotence()

    extra_total = (
        len(invariant_failures)
        + len(postal_failures)
        + len(search_key_failures)
        + len(fuzz_failures)
    )

    if failures or extra_total:
        if failures:
            print("\nVECTOR FAILURES:", file=sys.stderr)
            for failure in failures:
                print(failure, file=sys.stderr)
        if invariant_failures:
            print("\nINVARIANT FAILURES:", file=sys.stderr)
            for failure in invariant_failures:
                print(failure, file=sys.stderr)
        if latinish_failures:
            print("\nLATINISH MARK FAILURES:", file=sys.stderr)
            for failure in latinish_failures:
                print(failure, file=sys.stderr)
        if postal_failures:
            print("\nPOSTAL NORMALIZATION FAILURES:", file=sys.stderr)
            for failure in postal_failures:
                print(failure, file=sys.stderr)
        if search_key_failures:
            print("\nSEARCH KEY FAILURES:", file=sys.stderr)
            for failure in search_key_failures:
                print(failure, file=sys.stderr)
        if fuzz_failures:
            print("\nFUZZ FAILURES (seeded, reproducible):", file=sys.stderr)
            for failure in fuzz_failures:
                print(failure, file=sys.stderr)
        print(
            "\nDo not edit the fixture to make these pass. "
            "See CONTRIBUTING.md section 5.1.",
            file=sys.stderr,
        )
        return 1

    print(
        "invariants: idempotence, Latinish marks stripped, no control characters — OK\n"
        "postal_normalize: 8/8 cases OK\n"
        "search_key_unaccented: 5/5 cases OK\n"
        "fuzz: 2000 seeded mixed-script inputs idempotent — OK"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
