"""
Canonical normalization for TerraKit.

Implements core/spec/normalization.md, which is normative. Every SDK
(Swift, Kotlin) and the Rust CLI implement the same algorithm independently
and are checked against tests/fixtures/normalization.json.

This module is the Python implementation and the conformance oracle's backing
code. tests/conformance/run_normalization.py imports from here rather than
keeping a second copy — a duplicate Python implementation would drift, and
drift in normalization means the SDKs return different results for the same
query.

If you believe this is wrong, do not edit it. Edit the spec, then update the
fixture, then update every implementation. See CONTRIBUTING.md section 5.1.
"""

from __future__ import annotations

import unicodedata

__all__ = ["normalize", "postal_normalize", "SEARCH_KEY_MIN_LENGTH"]

# FTS5 trigram requires 3+ characters to produce a token. Queries shorter than
# this must be routed to the idx_places_norm B-tree prefix index.
# See DATA_MODEL.md section 9.
SEARCH_KEY_MIN_LENGTH = 3

# core/spec/normalization.md section 3.
# Applied after NFKD + mark stripping, BEFORE case folding.
# Order is significant and is fixed by the spec.
SPECIAL_CASES: dict[str, str] = {
    # German sharp s: the capital is a distinct code point (U+1E9E)
    "ß": "ss",
    "ẞ": "ss",
    "æ": "ae",
    "Æ": "ae",
    "ǽ": "ae",
    "Ǽ": "ae",
    "œ": "oe",
    "Œ": "oe",
    "ø": "o",
    "Ø": "o",
    "đ": "d",
    "Đ": "d",
    # eth and thorn are distinct letters, not variants of d and t
    "ð": "d",
    "Ð": "d",
    "þ": "th",
    "Þ": "th",
    "ł": "l",
    "Ł": "l",
    # Turkish dotted and dotless I must fold together or Istanbul is
    # unfindable from one form or the other (spec section 3).
    "ı": "i",
    "İ": "i",
    "ħ": "h",
    "Ħ": "h",
    "ŋ": "ng",
    "Ŋ": "ng",
    "ŧ": "t",
    "Ŧ": "t",
    "ə": "e",
    "Ə": "e",
    # Soft hyphen: survives NFKD and case folding, invisible, breaks lookups
    # in text pasted from word processors. Removed, not spaced.
    "­": "",
    # Dashes normalize to '-' first so the separator rule handles them.
    "‐": "-",
    "‑": "-",
    "‒": "-",
    "–": "-",
    "—": "-",
    "―": "-",
    "−": "-",
    # Apostrophes are REMOVED, not converted to a space (spec section 6):
    # D'Angelo -> dangelo, not "d angelo".
    "'": "",
    "’": "",
    "‘": "",
    "ʼ": "",
    "`": "",
    " ": " ",
}

MARK_CATEGORIES = frozenset(("Mn", "Mc", "Me"))

# Unicode name prefixes of scripts whose combining marks are load-bearing and
# must NOT be stripped.
#
# NFKD decomposes Japanese kana carrying a voiced sound mark (dakuten) or a
# semi-voice mark (handakuten) into a base kana plus a combining mark of category
# Mn:
#
#     ガ → 'カ' + U+3099     ガザ → 'カサ'    (Gaza became "Kasa")
#     バ → 'ハ' + U+3099     ヴ  → 'ウ' + U+3098
#     パ → 'ハ' + U+309A
#
# A blanket "strip every Mn" therefore rewrites Japanese place names into
# *different* place names, and worse, collapses distinct ones onto a single key:
# カ and ガ both normalize to カ. Japan is an MVP country (spec section 52), so
# this is a correctness bug, not a cosmetic one. It was found by building the
# first Japanese package and watching a Japanese search return nothing.
#
# Stripping is therefore restricted to marks that sit on a Latin, Greek or
# Cyrillic base letter — which is every case the step exists for
# (é → e, İ → I, ё → е) — and marks elsewhere are preserved.
_LATINISH_SCRIPTS = frozenset({"LATIN", "GREEK", "CYRILLIC"})


def _strip_latinish_marks(text: str) -> str:
    """
    Remove combining marks that modify a Latin, Greek or Cyrillic base letter.

    Walks the decomposed string rather than filtering on category, because the
    category cannot say whether the mark is meaningful. U+0307 above 'I' is
    decoration and must go, or 'İstanbul' and 'Istanbul' diverge; U+3099 above
    'カ' is the difference between two kana and must stay, or 'ガザ' becomes a
    different city.
    """
    out: list[str] = []
    base_is_latinish = False
    for char in text:
        if unicodedata.category(char) in MARK_CATEGORIES:
            if not base_is_latinish:
                out.append(char)
            continue
        try:
            script = unicodedata.name(char).split(" ", 1)[0]
        except ValueError:
            script = ""
        base_is_latinish = script in _LATINISH_SCRIPTS
        out.append(char)
    return "".join(out)


def normalize(input: str) -> str:
    """
    core/spec/normalization.md — normative algorithm.

    The ONLY normalization an SDK performs at query time. Everything else
    (phonetic codes, transliterations, search ranks) is precomputed by the
    pipeline and shipped inside the package. See ADR-003.
    """
    # 1. NFKD decomposition. Compatibility forms matter: the ligature in
    #    'final' has no canonical decomposition, so NFD would miss it.
    text = unicodedata.normalize("NFKD", input)

    # 2. Strip combining marks, but only those on Latin/Greek/Cyrillic bases.
    #    See _strip_latinish_marks: stripping every Mn destroys Japanese kana.
    text = _strip_latinish_marks(text)

    # 3. Special-case substitutions.
    text = "".join(SPECIAL_CASES.get(c, c) for c in text)

    # 4. Full Unicode case folding. str.casefold() implements CaseFolding.txt
    #    F + C mappings. Turkic (T) mappings are deliberately not applied.
    text = text.casefold()

    # 5. Transliteration is pipeline-only. SDKs never transliterate
    #    (DATA_MODEL.md section 5.2), so Cyrillic, Georgian and CJK pass
    #    through unchanged here. Cross-script search happens through
    #    precomputed place_alternate_names rows.

    # 6-8. Separator handling: runs of non-letter/non-number become one space,
    #      then trim, then collapse internal runs.
    #
    # Combining marks count as word-internal. Any mark still present at this point
    # survived step 2, which means it is load-bearing — a Japanese dakuten — and
    # treating it as a separator turned ガザ into 'カ サ'. Marks on Latinish bases
    # were already removed, so nothing reaches here that is mere decoration.
    out: list[str] = []
    pending_space = False
    for ch in text:
        category = unicodedata.category(ch)
        if category[0] in ("L", "N", "M"):
            if pending_space and out:
                out.append(" ")
            out.append(ch)
            pending_space = False
        else:
            pending_space = True
    result = "".join(out)

    # 9. Recompose to NFC.
    #
    # NFKD leaves Hangul as decomposed jamo — '서울' becomes 12 code points
    # instead of 3 — and leaves Japanese kana in their decomposed two-code-point
    # form. Both are self-consistent, so search would work, but the keys are
    # larger than they need to be and two textually identical names written by
    # different tools can differ in form.
    #
    # DATA_MODEL.md section 30 requires 'NFC-stable after NFKD', which is exactly
    # this. It also makes idempotence hold trivially.
    return unicodedata.normalize("NFC", result)


def postal_normalize(code: str) -> str:
    """
    Stricter postal-code normalization, DATA_MODEL.md section 8.

    Letters and digits only, no separator collapse. So 'SW1A 1AA' and
    'SW1A1AA' both reach 'sw1a1aa', and '07160' keeps its leading zero.

    Using the generic normalizer on a postal code is a bug: it preserves
    internal spaces, so the key will not match the indexed value.
    """
    text = unicodedata.normalize("NFKC", code)
    return "".join(c for c in text if c.isalnum()).casefold()


def search_key_unaccented(name: str) -> str:
    """
    Second trigram column: name with ALL separators removed.

    Motivation is concrete. A user typing 'AtatürkCaddesi' as one token, or a
    CJK place name with no spaces, produces a query the space-collapsing
    normalizer cannot match against 'ataturk caddesi'. Stripping separators
    entirely gives 'ataturkcaddesi', which trigram-matches both forms.

    This is not the same as name_norm, which is why search_fts carries both.
    """
    return normalize(name).replace(" ", "")
