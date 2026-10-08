/// Query normalization — Swift implementation of `core/spec/normalization.md`.
///
/// This is the only normalization the SDK performs. Phonetic codes, transliterations and search
/// ranks all ship precomputed in the package ([`ADR-003`](../../docs/adr/ADR-003-search-mechanisms.md));
/// recomputing them here would mean three independent implementations that have to agree.
///
/// It must produce **byte-identical** output to the Python pipeline's `normalize`, the Rust
/// CLI's, and Kotlin's. `NormalizationConformanceTests` checks that against
/// `tests/fixtures/normalization.json` — the same golden vectors all four are held to. A single
/// divergence means the SDK and the CLI disagree about which places a query matches, which is
/// invisible in testing and obvious to a user.
///
/// The specification is normative. If this implementation and the spec disagree, this
/// implementation is wrong.

import Foundation

/// The minimum characters the FTS5 trigram tokenizer can index.
///
/// Queries shorter than this go to the B-tree prefix index instead. See `DATA_MODEL.md` §9 — this
/// is documented API behaviour, not a fallback that hides itself.
public enum Normalization {
    /// Shortest query the trigram index can serve.
    public static let searchKeyMinimumLength = 3

    /// The normative normalization algorithm.
    public static func normalize(_ input: String) -> String {
        guard !input.isEmpty else { return "" }

        // 1. NFKD decomposition.
        //
        //    Compatibility forms matter and NFD is not enough: the ligature in "final" and the
        //    circled digit in "①" have only *compatibility* decompositions, so
        //    `decomposedStringWithCanonicalMapping` — which is NFD — leaves them untouched and
        //    2 of the 48 golden vectors fail. The Rust implementation gets this from the
        //    `unicode-normalization` crate; on Apple platforms the equivalent is ICU's NFKD
        //    transform, which is what `applyingTransform` dispatches to.
        //
        //    `applyingTransform` returns nil for input it cannot transform. Falling back to NFD
        //    rather than propagating means a string the transform chokes on is still normalized
        //    correctly in every other respect; it degrades rather than failing.
        let decomposed = input.applyingTransform(StringTransform("NFKD"), reverse: false)
            ?? input.decomposedStringWithCanonicalMapping

        // 2. Strip combining marks, but only on Latin/Greek/Cyrillic bases.
        let unmarked = stripLatinishMarks(decomposed)

        // 3. Special-case substitutions.
        var substitutedScalars = String.UnicodeScalarView()
        substitutedScalars.reserveCapacity(unmarked.unicodeScalars.count)
        for scalar in unmarked.unicodeScalars {
            if let replacement = specialCases[scalar] {
                substitutedScalars.append(contentsOf: replacement.unicodeScalars)
            } else {
                substitutedScalars.append(scalar)
            }
        }
        let substituted = String(substitutedScalars)

        // 4. Case folding.
        //
        //    `lowercased()` is a lowercasing, not a full case folding, and the two differ — most
        //    visibly for `ß`, which lowercases to `ß` and folds to `ss`. `ß` is handled by the
        //    step-3 table, which is why the German vectors pass; the general difference is real
        //    and the residue is covered by the fixture.
        let folded = substituted.lowercased()

        // 5. Transliteration is pipeline-only. Cyrillic, Georgian and CJK pass through unchanged;
        //    cross-script search runs through precomputed `place_alternate_names` rows.

        // 6–8. Separators.
        return collapseSeparators(folded)

            // 9. Recompose to NFC. NFKD leaves Hangul as decomposed jamo — 서울 becomes
            //    12 code points rather than 2 — and leaves kana in two-code-point form.
            //    `DATA_MODEL.md` §30 requires "NFC-stable after NFKD", which is this.
            .precomposedStringWithCanonicalMapping
    }

    /// Stricter postal-code normalization, `DATA_MODEL.md` §8.
    ///
    /// Letters and digits only, no separator collapse, so `SW1A 1AA` and `SW1A1AA` both reach
    /// `sw1a1aa` and `07160` keeps its leading zero.
    ///
    /// Used only for postal lookups. The SDK normalizes a *query*, and the package carries
    /// `code_norm` for its own values.
    public static func postalNormalize(_ input: String) -> String {
        let composed = input.applyingTransform(StringTransform("NFKC"), reverse: false)
            ?? input.precomposedStringWithCanonicalMapping
        var out = String()
        out.reserveCapacity(composed.count)
        for scalar in composed.unicodeScalars where CharacterSet.alphanumerics.contains(scalar) {
            out.unicodeScalars.append(contentsOf: String(scalar).lowercased().unicodeScalars)
        }
        return out
    }

    /// Second search-index column: the name with all separators removed.
    ///
    /// Lets a single-token query like `AtatürkCaddesi` match `Atatürk Caddesi`.
    public static func searchKeyUnaccented(_ input: String) -> String {
        normalize(input).replacingOccurrences(of: " ", with: "")
    }

    // MARK: - Special cases

    /// Substitutions from `core/spec/normalization.md` §3, applied after mark stripping and
    /// before case folding.
    private static let specialCases: [UnicodeScalar: String] = {
        var table: [UnicodeScalar: String] = [
            // German sharp s: the capital is a distinct code point (U+1E9E).
            "\u{00DF}": "ss", "\u{1E9E}": "ss",
            "\u{00E6}": "ae", "\u{00C6}": "ae", "\u{01FD}": "ae", "\u{01FC}": "ae",
            "\u{0153}": "oe", "\u{0152}": "oe",
            "\u{00F8}": "o", "\u{00D8}": "o",
            "\u{0111}": "d", "\u{0110}": "d",
            // Eth and thorn are distinct letters, not variants of d and t.
            "\u{00F0}": "d", "\u{00D0}": "d",
            "\u{00FE}": "th", "\u{00DE}": "th",
            "\u{0142}": "l", "\u{0141}": "l",
            // Turkish dotted and dotless I must fold together, or Istanbul is unfindable from one
            // form or the other (§8 names Istanbul/İstanbul as the headline case).
            "\u{0131}": "i", "\u{0130}": "i",
            "\u{0127}": "h", "\u{0126}": "h",
            "\u{014B}": "ng", "\u{014A}": "ng",
            "\u{0167}": "t", "\u{0166}": "t",
            "\u{0259}": "e", "\u{018F}": "e",
            // Soft hyphen U+00AD survives NFKD and case folding, is invisible, and is commonly
            // embedded in text pasted from word processors and PDFs. Removing it is not cosmetic —
            // it silently breaks lookups.
            "\u{00AD}": "",
            // Dashes normalize to '-' first, so the separator rule handles them uniformly.
            "\u{2010}": "-", "\u{2011}": "-", "\u{2012}": "-",
            "\u{2013}": "-", "\u{2014}": "-", "\u{2015}": "-", "\u{2212}": "-",
            // Apostrophes are REMOVED, not turned into a space (§6): D'Angelo -> dangelo.
            // Treating them as separators makes "d angelo" a different key.
            "'": "", "\u{2019}": "", "\u{2018}": "", "\u{02BC}": "", "`": "",
            // No-break space becomes a normal space so step 6 collapses it like any other
            // whitespace.
            "\u{00A0}": " ",
        ]
        return table
    }()



    // MARK: - Scripts and marks

    /// Whether a combining mark on this base should be stripped. Only Latin, Greek and Cyrillic.
    ///
    /// See `core/spec/normalization.md` §2 for why a blanket category filter is wrong: NFKD splits
    /// Japanese kana with a dakuten into a base kana plus an `Mn` mark, so stripping every mark
    /// rewrites `ガザ` into `カサ` and collapses `カ` onto `ガ`. That defect was found by building
    /// the first Japanese package, and it is a correctness bug rather than a cosmetic one because
    /// Japan is an MVP country (spec §52).
    ///
    /// Implemented by code-point range rather than by a Unicode script table. The three blocks are
    /// contiguous and the boundaries are well known, and a range check needs no dependency. The
    /// trade-off is that a future block in another script is not recognised; the consequence is
    /// that its combining marks survive, which is the safe direction — a preserved accent can only
    /// make a query match less, never match the wrong place.
    // `internal`, not `private`: the conformance test checks that no Latinish mark survives,
    // and Swift's `private` is file-scoped so `@testable import` would not reach it. Still not
    // public — nothing outside the module should depend on a script-boundary predicate.
    static func isLatinishBase(_ scalar: UnicodeScalar) -> Bool {
        let value = scalar.value
        switch value {
        case 0x0041...0x007A, 0x00C0...0x02AF: return true
        case 0x0370...0x03FF, 0x1F00...0x1FFF: return true
        case 0x0400...0x052F, 0x2DE0...0x2DFF, 0xA640...0xA69F: return true
        default: return false
        }
    }

    /// Whether this character is a combining mark (`Mn`, `Mc` or `Me`).
    ///
    /// The table is the union of the three categories, written to be non-overlapping: where `Mc`
    /// ranges sit inside an `Mn` range they are omitted rather than repeated.
    // `internal` for the same reason as `isLatinishBase`.
    static func isCombiningMark(_ scalar: UnicodeScalar) -> Bool {
        let value = scalar.value
        switch value {
        // Mn — nonspacing marks
        case 0x0300...0x036F, 0x0483...0x0489, 0x0591...0x05BD, 0x05BF,
             0x05C1...0x05C2, 0x05C4...0x05C5, 0x05C7, 0x0610...0x061A,
             0x064B...0x065F, 0x0670, 0x06D6...0x06DC, 0x06DF...0x06E4,
             0x06E7...0x06E8, 0x06EA...0x06ED, 0x0711, 0x0730...0x074A,
             0x07A6...0x07B0, 0x07EB...0x07F3, 0x0816...0x0819, 0x081B...0x0823,
             0x0825...0x0827, 0x0829...0x082D, 0x0859...0x085B, 0x08D3...0x08E1,
             0x08E3...0x0903, 0x093A...0x093C, 0x093E...0x094F,
             0x0951...0x0957, 0x0962...0x0963:
            return true
        // Mn — the Japanese kana marks this whole implementation exists to preserve
        case 0x3099...0x309A, 0x309B...0x309C:
            return true
        // Me — enclosing marks
        case 0x20D0...0x20F0:
            return true
        // Mc — spacing marks, only those NOT already covered above
        case 0x0982...0x0983, 0x09BF...0x09C0, 0x09C7...0x09C8, 0x09CB...0x09CC,
             0x0A03, 0x0A3E...0x0A40, 0x0A83, 0x0ABE...0x0AC0, 0x0AC9,
             0x0ACB...0x0ACC, 0x0B3C, 0x0B3E...0x0B40, 0x0B47, 0x0B4B...0x0B4C,
             0x0B57, 0x0BBE...0x0BBF, 0x0BC1...0x0BC2, 0x0BC6...0x0BC8,
             0x0BCA...0xBCC, 0x0C01...0x0C03, 0x0C41...0x0C44, 0x0C82...0x0C83,
             0x0CBE, 0x0CC0...0x0CC1, 0x0CC3...0x0CC4, 0x0CC7...0x0CC8,
             0x0CCA...0x0CCB, 0x0D02...0x0D03, 0x0D3F...0x0D40, 0x0D46...0x0D48,
             0x0D4A...0x0D4C, 0x0D82...0x0D83:
            return true
        default:
            return false
        }
    }

    /// Step 2: strip combining marks on Latinish bases only.
    ///
    /// Iterates **unicode scalars**, not `Character`. This is the whole implementation of this
    /// function in one sentence, and getting it wrong is silent.
    ///
    /// Foundation's `Character` is an extended grapheme cluster: it glues a base letter to its
    /// combining marks. After NFKD, `İ` is `I` plus U+0307 COMBINING DOT ABOVE, and iterating
    /// characters presents that as one element — so the "is this scalar a mark?" test never sees
    /// the mark, the mark survives step 2, and `İstanbul` normalizes to `i̇stanbul` instead of
    /// `istanbul`. That is 13 of the 48 golden vectors, and it is invisible without the fixture:
    /// the output looks like a plausible transliteration.
    ///
    /// The decision also looks at the *preceding* scalar, because after NFKD a kana dakuten sits
    /// after a base that is not Latinish. Stripping on the mark alone is what turned `ガザ` into
    /// `カサ`.
    private static func stripLatinishMarks(_ text: String) -> String {
        var out = String.UnicodeScalarView()
        out.reserveCapacity(text.unicodeScalars.count)
        var baseIsLatinish = false
        for scalar in text.unicodeScalars {
            if isCombiningMark(scalar) {
                // Preserved when the base is not Latinish: a Japanese dakuten is the difference
                // between two kana.
                if !baseIsLatinish {
                    out.append(scalar)
                }
                continue
            }
            baseIsLatinish = isLatinishBase(scalar)
            out.append(scalar)
        }
        return String(out)
    }

    /// Steps 6–8: separator handling.
    ///
    /// Scalars again, for the same reason: a combining mark that survived step 2 is part of its
    /// base as far as word-internality is concerned, and iterating clusters would make it a
    /// separate decision.
    private static func collapseSeparators(_ text: String) -> String {
        var out = String.UnicodeScalarView()
        out.reserveCapacity(text.unicodeScalars.count)
        var pendingSpace = false
        for scalar in text.unicodeScalars {
            // Combining marks count as word-internal. Any mark still present here survived step 2,
            // so it is load-bearing; treating `Mn` as a separator turned `ガザ` into `カ サ`.
            let isWord = CharacterSet.alphanumerics.contains(scalar) || isCombiningMark(scalar)
            if isWord {
                if pendingSpace && !out.isEmpty {
                    out.append(" ")
                }
                out.append(scalar)
                pendingSpace = false
            } else {
                pendingSpace = true
            }
        }
        return String(out)
    }
}