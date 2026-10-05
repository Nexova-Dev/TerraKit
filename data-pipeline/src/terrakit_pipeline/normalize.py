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
    "ǽ": "ae",
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

    # 2. Strip combining marks.
    text = "".join(c for c in text if unicodedata.category(c) not in MARK_CATEGORIES)

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
