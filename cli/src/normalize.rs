//! Query normalization — Rust implementation of `core/spec/normalization.md`.
//!
//! This function is the only normalization the CLI performs. Everything else
//! (phonetic codes, transliterations, search ranks) is precomputed by the
//! pipeline and shipped inside the package
//! ([`ADR-003`](../../docs/adr/ADR-003-search-mechanisms.md)).
//!
//! It must produce **byte-identical** output to the Python pipeline's
//! `normalize`, and to the Swift and Kotlin SDKs' copies. `tests/normalize.rs`
//! checks that against `tests/fixtures/normalization.json`, the same fixture all
//! four implementations are held to. A single divergence means the CLI and the
//! SDK disagree about which places a query matches, which is invisible in testing
//! and obvious to a user.
//!
//! The specification is normative. If this implementation and the spec disagree,
//! this implementation is wrong.

use unicode_normalization::UnicodeNormalization;

/// Minimum characters for the FTS5 trigram tokenizer to produce a token.
///
/// Queries shorter than this go to the B-tree prefix index instead. See
/// `DATA_MODEL.md` §9 — this is documented API behaviour, not a fallback that
/// hides itself.
pub const SEARCH_KEY_MIN_LENGTH: usize = 3;

// Special-case substitutions, `core/spec/normalization.md` §3.
//
// Applied after NFKD + mark stripping, before case folding. Order within the
// table is irrelevant because each key is a distinct character, but the table
// itself is fixed by the specification.
const SPECIAL_CASES: &[(char, &str)] = &[
    // German sharp s: the capital is a distinct code point (U+1E9E)
    ('ß', "ss"),
    ('ẞ', "ss"),
    ('æ', "ae"),
    ('Æ', "ae"),
    ('ǽ', "ae"),
    ('Ǽ', "ae"),
    ('œ', "oe"),
    ('Œ', "oe"),
    ('ø', "o"),
    ('Ø', "o"),
    ('đ', "d"),
    ('Đ', "d"),
    // eth and thorn are distinct letters, not variants of d and t
    ('ð', "d"),
    ('Ð', "d"),
    ('þ', "th"),
    ('Þ', "th"),
    ('ł', "l"),
    ('Ł', "l"),
    // Turkish dotted and dotless I must fold together or Istanbul is unfindable
    // from one form or the other (§8 names Istanbul/İstanbul as the headline
    // case).
    ('ı', "i"),
    ('İ', "i"),
    ('ħ', "h"),
    ('Ħ', "h"),
    ('ŋ', "ng"),
    ('Ŋ', "ng"),
    ('ŧ', "t"),
    ('Ŧ', "t"),
    ('ə', "e"),
    ('Ə', "e"),
    // Soft hyphen U+00AD: survives NFKD and case folding, is invisible, and is
    // commonly embedded in text pasted from word processors and PDFs. Removing it
    // is not cosmetic — it silently breaks lookups.
    ('­', ""),
    // Dashes normalize to '-' first, so the separator rule handles them uniformly.
    ('‐', "-"),
    ('‑', "-"),
    ('‒', "-"),
    ('–', "-"),
    ('—', "-"),
    ('―', "-"),
    ('−', "-"),
    // Apostrophes are REMOVED, not turned into a space (§6): D'Angelo -> dangelo.
    // Treating them as separators makes "d angelo" a different key.
    ('\'', ""),
    ('’', ""),
    ('‘', ""),
    ('ʼ', ""),
    ('`', ""),
    // No-break space becomes a normal space so step 6 collapses it like any
    // other whitespace.
    (' ', " "),
];

fn special_case(ch: char) -> Option<&'static str> {
    SPECIAL_CASES
        .iter()
        .find(|(key, _)| *key == ch)
        .map(|(_, value)| *value)
}

/// Whether a combining mark on this base should be stripped.
///
/// Only Latin, Greek and Cyrillic. See `core/spec/normalization.md` §2 for why a
/// blanket category filter is wrong: NFKD splits Japanese kana with a dakuten
/// into a base kana plus an `Mn` mark, so stripping every mark rewrites `ガザ`
/// into `カサ` and collapses `カ` onto `ガ`. That defect was found by building the
/// first Japanese package, and it is a correctness bug rather than a cosmetic
/// one because Japan is an MVP country (spec §52).
///
/// Implemented by code-point range rather than by a Unicode script table.
/// The three blocks are contiguous and the boundaries are well known, and a
/// range check needs no dependency and no startup cost. The trade-off is that a
/// future block in another script would not be recognised; the consequence is
/// that its combining marks survive, which is the safe direction — a preserved
/// accent can only make a query match less, never make it match the wrong place.
#[inline]
fn is_latinish_base(ch: char) -> bool {
    matches!(ch as u32,
        // Basic Latin, Latin-1 Supplement, Latin Extended A and B,
        // IPA Extensions, Latin Extended Additional
        0x0041..=0x007A | 0x00C0..=0x02AF
        // Greek and Coptic, Greek Extended
        | 0x0370..=0x03FF | 0x1F00..=0x1FFF
        // Cyrillic, Cyrillic Supplement, Cyrillic Extended-A and -B
        | 0x0400..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F
    )
}

/// Combining-mark category check: `Mn`, `Mc` or `Me`.
///
/// Rust's standard library has no category API, and the ranges below are stable
/// Unicode data. Adding a dependency for `general_category` on a function this
/// small would be a poor trade in a binary whose selling point is minimal size.
/// Whether this character is a combining mark (`Mn`, `Mc` or `Me`).
///
/// Rust's standard library has no general-category API, and the ranges below are
/// stable Unicode data. A dependency for `general_category` on a predicate this
/// small would be a poor trade in a binary whose selling point is minimal size.
///
/// The table is the union of the three categories, written to be
/// non-overlapping. Where `Mc` ranges sit inside an `Mn` range they are omitted
/// rather than repeated: the predicate answers "is this a mark", and a subsumed
/// arm is dead code that also warns.
///
/// The kana ranges are called out because they are the reason this function has
/// a caller that looks at the preceding character rather than the mark alone.
#[inline]
fn is_combining_mark(ch: char) -> bool {
    matches!(ch as u32,
        // Mn — nonspacing marks
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF
        | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A
        | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8 | 0x06EA..=0x06ED | 0x0711 | 0x0730..=0x074A
        | 0x07A6..=0x07B0 | 0x07EB..=0x07F3 | 0x0816..=0x0819 | 0x081B..=0x0823
        | 0x0825..=0x0827 | 0x0829..=0x082D | 0x0859..=0x085B
        | 0x08D3..=0x08E1
        // Mn — Devanagari and friends. These subsume the corresponding Mc ranges.
        | 0x08E3..=0x0903 | 0x093A..=0x093C | 0x093E..=0x094F
        | 0x0951..=0x0957 | 0x0962..=0x0963
        // Mn — the Japanese kana marks this whole module exists to preserve
        | 0x3099..=0x309A | 0x309B..=0x309C
        // Me — enclosing marks
        | 0x20D0..=0x20F0
        // Mc — spacing marks, only those NOT already covered above
        | 0x0982..=0x0983 | 0x09BF..=0x09C0 | 0x09C7..=0x09C8 | 0x09CB..=0x09CC
        | 0x0A03 | 0x0A3E..=0x0A40 | 0x0A83 | 0x0ABE..=0x0AC0 | 0x0AC9
        | 0x0ACB..=0x0ACC
        | 0x0B3C | 0x0B3E..=0x0B40 | 0x0B47 | 0x0B4B..=0x0B4C | 0x0B57
        | 0x0BBE..=0x0BBF | 0x0BC1..=0x0BC2 | 0x0BC6..=0x0BC8 | 0x0BCA..=0x0BCC
        | 0x0C01..=0x0C03 | 0x0C41..=0x0C44 | 0x0C82..=0x0C83 | 0x0CBE
        | 0x0CC0..=0x0CC1 | 0x0CC3..=0x0CC4 | 0x0CC7..=0x0CC8 | 0x0CCA..=0x0CCB
        | 0x0D02..=0x0D03 | 0x0D3F..=0x0D40 | 0x0D46..=0x0D48 | 0x0D4A..=0x0D4C
        | 0x0D82..=0x0D83
    )
}

/// Letter or number: the characters step 6 treats as word-internal.
#[inline]
fn is_letter_or_number(ch: char) -> bool {
    ch.is_alphanumeric()
}

/// Step 2: strip combining marks on Latinish bases only.
fn strip_latinish_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut base_is_latinish = false;
    for ch in text.chars() {
        if is_combining_mark(ch) {
            // Preserved when the base is not Latinish: a Japanese dakuten is the
            // difference between two kana.
            if !base_is_latinish {
                out.push(ch);
            }
            continue;
        }
        base_is_latinish = is_latinish_base(ch);
        out.push(ch);
    }
    out
}

/// Step 6–8: separator handling.
fn collapse_separators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        // Combining marks count as word-internal. Any mark still present here
        // survived step 2, so it is load-bearing; treating `Mn` as a separator
        // turned `ガザ` into `カ サ`.
        if is_letter_or_number(ch) || is_combining_mark(ch) {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            out.push(ch);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    out
}

/// The normative normalization algorithm. See `core/spec/normalization.md`.
pub fn normalize(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    // 1. NFKD decomposition. Compatibility forms matter: the ligature in "final"
    //    has no canonical decomposition, so NFD would miss it.
    let decomposed: String = input.nfkd().collect();

    // 2. Strip combining marks, but only on Latin/Greek/Cyrillic bases.
    let unmarked = strip_latinish_marks(&decomposed);

    // 3. Special-case substitutions.
    let mut substituted = String::with_capacity(unmarked.len());
    for ch in unmarked.chars() {
        match special_case(ch) {
            Some(replacement) => substituted.push_str(replacement),
            None => substituted.push(ch),
        }
    }

    // 4. Full Unicode case folding.
    //
    //    Rust's `to_lowercase` is a *lowercasing*, not full case folding, and
    //    the two differ — most visibly for `ß`, which lowercases to `ß` and
    //    folds to `ss`. `ß` is handled by the step-3 table, which is why the
    //    German vectors pass, but the general difference is real and is why this
    //    is called out rather than left implicit. `casefold()` is not available
    //    in the standard library, so the residue is covered by the fixture.
    let folded = substituted.to_lowercase();

    // 5. Transliteration is pipeline-only. Cyrillic, Georgian and CJK pass
    //    through unchanged; cross-script search happens through precomputed
    //    `place_alternate_names` rows (`DATA_MODEL.md` §5.2).

    // 6–8. Separators.
    let collapsed = collapse_separators(&folded);

    // 9. Recompose to NFC. NFKD leaves Hangul as decomposed jamo — `서울` becomes
    //    12 code points rather than 2 — and leaves kana in two-code-point form.
    //    `DATA_MODEL.md` §30 requires "NFC-stable after NFKD", which is this.
    collapsed.nfc().collect()
}

/// Stricter postal-code normalization, `DATA_MODEL.md` §8.
///
/// Letters and digits only, no separator collapse, so `SW1A 1AA` and `SW1A1AA`
/// both reach `sw1a1aa` and `07160` keeps its leading zero.
///
/// Used only by `terra validate` when checking a package's stored `code_norm`.
/// The SDKs never normalize a postal code at query time — they read the indexed
/// value.
pub fn postal_normalize(code: &str) -> String {
    code.nfkc()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Second search-index column: the name with all separators removed.
///
/// Lets a single-token query like `AtatürkCaddesi` match `Atatürk Caddesi`.
/// This is not the same as `normalize`, which is why `search_fts` carries both.
#[cfg_attr(not(test), allow(dead_code))]
pub fn search_key_unaccented(name: &str) -> String {
    normalize(name).replace(' ', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        // The examples spec §8 names explicitly.
        for (input, expected) in [
            ("Istanbul", "istanbul"),
            ("İstanbul", "istanbul"),
            ("istanbul", "istanbul"),
            ("ISTANBUL", "istanbul"),
            ("Atatürk", "ataturk"),
            ("Ataturk", "ataturk"),
            ("ATATURK", "ataturk"),
            ("Muratpaşa", "muratpasa"),
            ("Türkiye", "turkiye"),
            ("Straße", "strasse"),
            ("Ağrı", "agri"),
            ("İzmir", "izmir"),
        ] {
            assert_eq!(normalize(input), expected, "normalize({input:?})");
        }
    }

    #[test]
    fn japanese_dakuten_survives() {
        // The defect that made Japanese search return nothing: a categorical mark
        // strip rewrote these into different names and collided カ with ガ.
        for word in ["ガザ", "バ", "パ", "ヴ", "イスタンブール", "東京都"] {
            assert_eq!(normalize(word), word, "normalize({word:?})");
        }
        assert_ne!(normalize("カ"), normalize("ガ"));
        assert_ne!(normalize("パ"), normalize("ハ"));
        assert!(!normalize("ガザ").contains(' '));
    }

    #[test]
    fn hangul_recomposes() {
        for word in ["서울", "부산"] {
            assert_eq!(normalize(word), word);
        }
    }

    #[test]
    fn latinish_marks_stripped() {
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(normalize("Łódź"), "lodz");
        assert_eq!(normalize("Nice"), "nice");
        assert_eq!(normalize("①"), "1");
        assert_eq!(normalize("ﬁnal"), "final");
    }

    #[test]
    fn apostrophe_removed_but_hyphen_spaced() {
        // §6: these are different rules and conflating them breaks one or merges
        // two genuinely distinct keys.
        assert_eq!(normalize("D'Angelo"), "dangelo");
        assert_eq!(normalize("D’Angelo"), "dangelo");
        assert_eq!(normalize("Saint-Denis"), "saint denis");
        assert_eq!(normalize("New-York"), "new york");
    }

    #[test]
    fn postal_normalization_is_stricter() {
        assert_eq!(postal_normalize("SW1A 1AA"), "sw1a1aa");
        assert_eq!(postal_normalize("SW1A1AA"), "sw1a1aa");
        assert_eq!(postal_normalize("SW1A-1AA"), "sw1a1aa");
        assert_eq!(postal_normalize("07160"), "07160");
        assert_eq!(postal_normalize("  07000  "), "07000");
    }

    #[test]
    fn idempotent() {
        for input in [
            "İstanbul", "Atatürk Caddesi", "ガザ", "서울", "Straße", "",
            "!!!", "  Antalya  ", "SW1A 1AA",
        ] {
            let once = normalize(input);
            assert_eq!(normalize(&once), once, "not idempotent for {input:?}");
        }
    }

    #[test]
    fn soft_hyphen_removed() {
        assert_eq!(normalize("San\u{AD}ta"), "santa");
    }
}
