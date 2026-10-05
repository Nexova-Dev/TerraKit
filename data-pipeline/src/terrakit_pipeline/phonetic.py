"""
Double Metaphone — build-time phonetic index.

SDKs NEVER call this (ADR-003). The pipeline computes codes once and ships them
in `phonetic_codes`. Implementing Double Metaphone correctly in Swift, Kotlin and
Rust and keeping all three byte-identical is a liability with no upside;
agreement becomes a data property CI verifies once.

Scope, stated plainly so nobody assumes fidelity that is not here:

- Latin and extended Latin are folded to a phonetic alphabet. Turkish letters
  are explicit because Türkiye is an MVP country and ı/İ matter.
- Non-Latin scripts produce no codes. '東京' yields nothing, so it is not
  reachable by phonetic match — it is reachable through
  `place_alternate_names`, which the pipeline precomputes. Same division of
  labour as transliteration (DATA_MODEL.md section 5.2).
- Initial-position exceptions are preserved; they are what makes 'Xavier' and
  'Czar' behave.

Structure follows the original algorithm's mechanism: two simultaneous
encodings, because English and other Germanic languages diverge on the same
spelling ('AUTH' vs 'AUTHORITY').
"""

from __future__ import annotations

__all__ = ["double_metaphone", "VOWELS"]

VOWELS = frozenset("AEIOUY")

# Extended Latin and Turkish folded into the ASCII phonetic alphabet.
_PRIMARY: dict[str, str] = {
    # Turkish
    "Ç": "C", "Ğ": "G", "İ": "I", "I": "I", "ı": "I",
    "Ö": "O", "Ş": "S", "Ü": "U",
    # Western European
    "À": "A", "Á": "A", "Â": "A", "Ã": "A", "Å": "A", "Ä": "A",
    "Æ": "A", "È": "E", "É": "E", "Ê": "E", "Ë": "E",
    "Ì": "I", "Í": "I", "Î": "I", "Ï": "I", "Ð": "D", "Ñ": "N",
    "Ò": "O", "Ó": "O", "Ô": "O", "Õ": "O", "Ø": "O",
    "Ù": "U", "Ú": "U", "Û": "U", "Ý": "Y", "Ÿ": "Y",
    "Š": "S", "Ž": "S", "Ł": "L", "Þ": "T", "ß": "S",
    # Central European
    "Ā": "A", "Ă": "A", "Ą": "A", "Ć": "C", "Č": "C", "Ď": "D", "Đ": "D",
    "Ē": "E", "Ė": "E", "Ę": "E", "Ě": "E", "Ğ": "G",
    "Ī": "I", "Į": "I", "Ĺ": "L", "Ļ": "L",
    "Ń": "N", "Ň": "N", "Ō": "O", "Ő": "O", "Œ": "A",
    "Ŕ": "R", "Ř": "R", "Ś": "S", "Ş": "S",
    "Ť": "T", "Ū": "U", "Ů": "U", "Ű": "U", "Ų": "U",
    "Ź": "S", "Ż": "Z", "Ž": "S",
}

# Silent initial letters.
_SILENT_INITIAL = frozenset({"GN", "KN", "PN", "WR", "PS"})

# Consonants consumed without emitting.
_SKIP = frozenset({"H", "W"})

# H is silent after these, i.e. -CH, -SH, -PH, -TH, -GH.
_H_SILENT_FOLLOW = frozenset({"C", "S", "P", "T", "G"})

_VOWELS_OR_SILENT = frozenset({"A", "E", "I", "O", "U", "H", "W"})

MAX_CODE_LENGTH = 8


def _to_phonetic_alphabet(word: str) -> str:
    """Fold to uppercase ASCII-ish, dropping anything untranslatable."""
    out: list[str] = []
    for char in word.upper():
        if "A" <= char <= "Z":
            out.append(char)
        elif char in _PRIMARY:
            out.append(_PRIMARY[char])
        # Cyrillic, Greek, CJK, Georgian, punctuation: dropped by design.
    return "".join(out)


def double_metaphone(word: str) -> tuple[str, ...]:
    """
    Return up to two phonetic codes for `word`, deduplicated and ordered.

    Two codes because the same spelling genuinely has divergent pronunciations.
    """
    if not word or not word.strip():
        return ()

    text = _to_phonetic_alphabet(word)
    if len(text) < 2:
        # A single letter carries no usable phonetic signal; indexing it would
        # add rows without adding matches.
        return ()
    if text[0] in _SILENT_INITIAL:
        text = text[1:]
        if len(text) < 2:
            return ()

    primary, secondary = _encode(text)
    return tuple(dict.fromkeys(c for c in (primary, secondary) if c))


def _encode(text: str) -> tuple[str, str]:
    """Two simultaneous encodings."""
    primary: list[str] = []
    secondary: list[str] = []
    length = len(text)
    index = 0

    first = text[0]
    if first in ("X", "Z"):
        # 'Xavier' -> S, 'Xray' -> S, but 'X' as a name is KS. Keeping S here
        # is what makes those match.
        primary.append("S")
        secondary.append("S")
        index = 1

    while index < length:
        char = text[index]

        if char in _SKIP:
            index += _skip_width(text, index)
            continue

        if char in VOWELS:
            # Only an initial vowel is audible. A vowel anywhere else is
            # dropped: emitting "A" for every vowel turned 'Antalya' into
            # 'ANTALA' and 'Berlin' into 'PARLAN', which destroys the one
            # property this index exists for — that two spellings of the same
            # word collapse together.
            if index == 0:
                primary.append("A")
                secondary.append("A")
            index += 1
            continue

        handler = _RULES.get(char)
        if handler is None:
            primary.append(char)
            secondary.append(char)
            index += 1
            continue

        primary_codes, secondary_codes, consumed = handler(text, index)
        primary.extend(primary_codes)
        secondary.extend(secondary_codes)
        index += consumed if consumed > 0 else 1

    return ("".join(primary)[:MAX_CODE_LENGTH], "".join(secondary)[:MAX_CODE_LENGTH])


def _skip_width(text: str, index: int) -> int:
    """H and W are consumed silently, or pair with the letter after them."""
    following = text[index + 1] if index + 1 < len(text) else ""
    if text[index] == "H":
        if following in _H_SILENT_FOLLOW:
            return 2
        previous = text[index - 1] if index > 0 else ""
        if previous in VOWELS and following and following == previous:
            return 2
        return 1
    # W
    if following and following in VOWELS:
        return 2
    if index > 0 and text[index - 1] == "A" and following not in VOWELS:
        return 2
    return 1


# --- per-letter handlers ---------------------------------------------------
# Each returns (primary_codes, secondary_codes, chars_consumed).


def _r_c(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    nxt2 = w[i + 2] if i + 2 < len(w) else ""
    if nxt == "H":
        # Greek-origin spellings (CHORUS, SCHISM) are K; others are X.
        if i > 0 and nxt2 in VOWELS and w[i - 2] not in _VOWELS_OR_SILENT:
            return (["K"], ["K"], 2)
        if nxt2 and nxt2 not in VOWELS and w[i - 2 : i] not in ("BC", "GC", "PC", "SC", "TC", "NC", "RC"):
            return (["K"], ["K"], 2)
        return (["X"], ["X"], 2)
    if nxt in ("C", "S") and nxt2 in "EIY":
        return (["S"], ["X"], 3)
    if nxt in ("C", "I", "A"):
        return (["X"], ["X"], 3)
    if nxt in ("Z",) and not nxt2:
        return (["S", "X"], ["S", "X"], 2)
    if nxt == "C":
        return (["K"], ["K"], 2)
    if nxt in "KGQN":
        return (["K"], ["K"], 2)
    return (["K"], ["K"], 1)


def _r_d(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    if nxt == "G" and i + 2 < len(w) and w[i + 2] in "EYI":
        return (["J"], ["J"], 3)      # -DGE, -DGI, -DGY
    if nxt in "TK":
        return ([], [], 2)             # -DT, -DK: aspirated stops
    return (["T"], ["T"], 1)


def _r_g(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    nxt2 = w[i + 2] if i + 2 < len(w) else ""
    if nxt == "H":
        if i > 0 and nxt2 and nxt2 not in VOWELS:
            return (["K"], ["K"], 2)   # -GH at the end of a syllable
        if i == 0 and (not nxt2 or nxt2 not in VOWELS):
            return (["J"], ["J"], 2)   # initial -GH as in 'Ghent'
        if (
            (i >= 3 and w[i - 1] in "BDH" and w[i - 2] not in VOWELS)
            or (i >= 2 and w[i - 1] == "U" and w[i - 2] in "CSPTG")
        ):
            return (["F"], ["F"], 2)   # -UGH as in 'laugh'
        if i > 0 and w[i - 1] not in VOWELS:
            return (["K"], ["K"], 2)
        return (["J"], ["J"], 2)
    if nxt == "N":
        if i + 2 < len(w) and w[i + 2] in "EY" and nxt != "Y":
            return (["KN", "N"], ["N"], 2)   # -GNE, -GNY, -gni
        return ([], [], 2)
    if nxt in "LI" and nxt2 and nxt2 in "EY":
        return (["L", "L"], ["L"], 3)   # -GLI-, -GLIA-
    if nxt == "Y" and (i == 0 or w[i - 1] not in VOWELS):
        return (["K", "J"], ["J"], 2)
    return (["K"], ["K"], 1)


def _r_s(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    nxt2 = w[i + 2] if i + 2 < len(w) else ""
    if i > 0 and w[i - 1] not in VOWELS and nxt not in "AIY":
        return (["S"], ["X"], 2)       # German -SS-, -ß-
    if i == 0 and nxt in "MNLW" and nxt2 in VOWELS:
        return (["X", "S"], ["X"], 2)   # initial -S(M|N|L|W)V-
    if i + 1 >= len(w):
        return ([], [], 1)
    if nxt == "Z":
        return (["X"], ["S"], 2)       # Polish -SZ-
    if nxt == "S":
        return ([], [], 2)
    if nxt == "C":
        if nxt2 == "H":
            if i + 3 < len(w) and w[i + 3] in "OR":
                return (["K"], ["K"], 3)   # -SCHOR-, -SCHER-
            if i >= 2 and w[i - 1] in "AIYOUE":
                return (["X"], ["X"], 3)   # Dutch -ESCH-
            if w[i : i + 4] in ("TSCH", "TCH"):
                return (["X"], ["X"], 4)
            return (["K"], ["K"], 3)
        return (["S"], ["K"], 3)       # Italian -SCI-, -SCE-
    if nxt == "T":
        return (["X"], ["S"], 3)       # -TIO-, -TIA-, -TCH- handled above
    return (["S"], ["S"], 1)


def _r_t(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    nxt2 = w[i + 2] if i + 2 < len(w) else ""
    if nxt == "H":
        if nxt2 and nxt2 not in VOWELS:
            return (["0"], ["0"], 2)   # -TH as in 'thumb'
        return (["T"], ["T"], 2)
    if nxt in "TD":
        return ([], [], 2)             # -TT-, -TD-
    if w[i : i + 4] == "TSCH":
        return (["X"], ["X"], 4)
    if nxt == "C" and nxt2 == "H":
        return (["X"], ["X"], 3)
    return (["T"], ["T"], 1)


def _r_x(w: str, i: int) -> tuple[list[str], list[str], int]:
    # 'DIX' -> KS, 'AX' -> KS. A leading X was already handled as S.
    previous = w[i - 1] if i > 0 else ""
    if previous not in VOWELS:
        return (["S"], ["S"], 1)
    return (["KS"], ["KS"], 2)


def _r_p(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    if nxt == "H":
        return (["F"], ["F"], 2)
    if nxt in "PB":
        return ([], [], 2)
    return (["P"], ["P"], 1)


def _r_b(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    return ([], [], 2) if nxt == "B" else (["P"], ["P"], 1)


def _r_k(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    return ([], [], 2) if nxt == "K" else (["K"], ["K"], 1)


def _r_l(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    if nxt == "L":
        # 'L' before a vowel after another L is silent, otherwise it modifies.
        if i + 2 < len(w) and w[i + 2] in VOWELS:
            return ([], ["L"], 2)
        return (["L"], ["L"], 2)
    return (["L"], ["L"], 1)


def _r_m(w: str, i: int) -> tuple[list[str], list[str], int]:
    if (
        (i > 0 and w[i - 1] == "M" and w[i - 2 : i] == "UM")
        or (i > 1 and w[i - 1] == "U" and w[i - 2] == "M")
    ):
        return (["M"], ["M"], 2) if i > 1 and w[i - 1] == "M" else ([], ["M"], 2)
    return (["M"], ["M"], 1)


def _r_n(w: str, i: int) -> tuple[list[str], list[str], int]:
    nxt = w[i + 1] if i + 1 < len(w) else ""
    return ([], [], 2) if nxt == "N" else (["N"], ["N"], 1)


def _r_y(w: str, i: int) -> tuple[list[str], list[str], int]:
    if i == 0:
        return (["Y"], ["Y"], 1)
    previous = w[i - 1]
    following = w[i + 1] if i + 1 < len(w) else ""
    if previous in VOWELS:
        return (["Y"], ["Y"], 1)
    # A consonantal Y is silent.
    return ([], ["Y"], 1)


def _mk(*codes: str, consumed: int = 1):
    def handler(w: str, i: int):
        return (list(codes), list(codes), consumed)

    return handler


_RULES: dict[str, object] = {
    "B": _r_b,
    "C": _r_c,
    "D": _r_d,
    "F": _mk("F"),
    "G": _r_g,
    "K": _r_k,
    "L": _r_l,
    "M": _r_m,
    "N": _r_n,
    "P": _r_p,
    "Q": _mk("K"),
    "R": _mk("R"),
    "S": _r_s,
    "T": _r_t,
    "V": _mk("F"),
    "X": _r_x,
    "Z": _mk("S"),
    "Y": _r_y,
}
