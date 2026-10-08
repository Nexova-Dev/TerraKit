package com.terrakit.core

/**
 * Query normalization — the Kotlin implementation of `core/spec/normalization.md`.
 *
 * This is the only normalization the SDK performs. Phonetic codes, transliterations and search
 * ranks all ship precomputed in the package ([ADR-003][1]); recomputing them here would mean three
 * independent implementations that have to agree.
 *
 * It must produce **byte-identical** output to the Python pipeline's `normalize`, the Rust CLI's
 * and the Swift SDK's. `NormalizationConformanceTests` checks that against
 * `tests/fixtures/normalization.json` — the same golden vectors all four are held to. A single
 * divergence means the SDK and the CLI disagree about which places a query matches, which is
 * invisible in testing and obvious to a user.
 *
 * The specification is normative. If this implementation and the spec disagree, this implementation
 * is wrong.
 *
 * [1]: ../../../../docs/adr/ADR-003-search-mechanisms.md
 */
object Normalization {

    /** Shortest query the trigram index can serve. */
    const val SEARCH_KEY_MINIMUM_LENGTH = 3

    /** The normative normalization algorithm. */
    fun normalize(input: String): String {
        if (input.isEmpty()) return ""

        // 1. NFKD decomposition. Compatibility forms matter: the ligature in "final" and the circled
        //    digit in "①" have only *compatibility* decompositions, so a canonical decomposition
        //    leaves them untouched and two of the 48 golden vectors fail.
        val decomposed = java.text.Normalizer.normalize(input, java.text.Normalizer.Form.NFKD)

        // 2. Strip combining marks, but only on Latin/Greek/Cyrillic bases.
        val unmarked = stripLatinishMarks(decomposed)

        // 3. Special-case substitutions.
        val substituted = StringBuilder(unmarked.length)
        for (codePoint in unmarked.codePoints()) {
            val replacement = SPECIAL_CASES[codePoint]
            if (replacement != null) substituted.append(replacement) else substituted.appendCodePoint(codePoint)
        }

        // 4. Case folding.
        //
        //    `lowercase()` is a lowercasing, not a full case folding, and the two differ — most
        //    visibly for `ß`, which lowercases to `ß` and folds to `ss`. `ß` is handled by the
        //    step-3 table, which is why the German vectors pass; the general difference is real and
        //    the residue is covered by the fixture.
        val folded = substituted.toString().lowercase()

        // 5. Transliteration is pipeline-only. Cyrillic, Georgian and CJK pass through unchanged;
        //    cross-script search runs through precomputed `place_alternate_names` rows.

        // 6–8. Separators.
        val collapsed = collapseSeparators(folded)

        // 9. Recompose to NFC. NFKD leaves Hangul as decomposed jamo — 서울 becomes
        //    12 code points rather than 2 — and leaves kana in two-code-point form.
        //    `DATA_MODEL.md` §30 requires "NFC-stable after NFKD", which is this.
        return java.text.Normalizer.normalize(collapsed, java.text.Normalizer.Form.NFC)
    }

    /**
     * Stricter postal-code normalization, `DATA_MODEL.md` §8.
     *
     * Letters and digits only, no separator collapse, so `SW1A 1AA` and `SW1A1AA` both reach
     * `sw1a1aa` and `07160` keeps its leading zero.
     *
     * Used only for postal lookups. The SDK normalizes a *query*, and the package carries `code_norm`
     * for its own values.
     */
    fun postalNormalize(input: String): String {
        val composed = java.text.Normalizer.normalize(input, java.text.Normalizer.Form.NFKC)
        val out = StringBuilder(composed.length)
        composed.codePoints().forEach { codePoint ->
            if (Character.isLetterOrDigit(codePoint)) {
                out.append(String(Character.toChars(codePoint)).lowercase())
            }
        }
        return out.toString()
    }

    /**
     * Second search-index column: the name with all separators removed.
     *
     * Lets a single-token query like `AtatürkCaddesi` match `Atatürk Caddesi`.
     */
    fun searchKeyUnaccented(input: String): String = normalize(input).replace(" ", "")

    // MARK: - Special cases

    /**
     * Substitutions from `core/spec/normalization.md` §3, applied after mark stripping and before
     * case folding.
     */
    private val SPECIAL_CASES: Map<Int, String> = buildMap {
        // German sharp s: the capital is a distinct code point (U+1E9E).
        put(0x00DF, "ss"); put(0x1E9E, "ss")
        put(0x00E6, "ae"); put(0x00C6, "ae"); put(0x01FD, "ae"); put(0x01FC, "ae")
        put(0x0153, "oe"); put(0x0152, "oe")
        put(0x00F8, "o"); put(0x00D8, "o")
        put(0x0111, "d"); put(0x0110, "d")
        // Eth and thorn are distinct letters, not variants of d and t.
        put(0x00F0, "d"); put(0x00D0, "d")
        put(0x00FE, "th"); put(0x00DE, "th")
        put(0x0142, "l"); put(0x0141, "l")
        // Turkish dotted and dotless I must fold together, or Istanbul is unfindable from one form
        // or the other (§8 names Istanbul/İstanbul as the headline case).
        put(0x0131, "i"); put(0x0130, "i")
        put(0x0127, "h"); put(0x0126, "h")
        put(0x014B, "ng"); put(0x014A, "ng")
        put(0x0167, "t"); put(0x0166, "t")
        put(0x0259, "e"); put(0x018F, "e")
        // Soft hyphen U+00AD survives NFKD and case folding, is invisible, and is commonly embedded
        // in text pasted from word processors and PDFs. Removing it is not cosmetic — it silently
        // breaks lookups.
        put(0x00AD, "")
        // Dashes normalize to '-' first, so the separator rule handles them uniformly.
        put(0x2010, "-"); put(0x2011, "-"); put(0x2012, "-")
        put(0x2013, "-"); put(0x2014, "-"); put(0x2015, "-"); put(0x2212, "-")
        // Apostrophes are REMOVED, not turned into a space (§6): D'Angelo -> dangelo. Treating them
        // as separators makes "d angelo" a different key.
        put('\''.code, ""); put(0x2019, ""); put(0x2018, ""); put(0x02BC, ""); put('`'.code, "")
        // No-break space becomes a normal space so step 6 collapses it like any other whitespace.
        put(0x00A0, " ")
    }

    // MARK: - Scripts and marks

    /**
     * Whether a combining mark on this base should be stripped. Only Latin, Greek and Cyrillic.
     *
     * See `core/spec/normalization.md` §2 for why a blanket category filter is wrong: NFKD splits
     * Japanese kana with a dakuten into a base kana plus an `Mn` mark, so stripping every mark
     * rewrites `ガザ` into `カサ` and collapses `カ` onto `ガ`. That defect was found by building the
     * first Japanese package, and it is a correctness bug rather than a cosmetic one because Japan
     * is an MVP country (spec §52).
     *
     * Implemented by code-point range rather than by a Unicode script table. The three blocks are
     * contiguous and the boundaries are well known, and a range check needs no dependency. The
     * trade-off is that a future block in another script is not recognised; the consequence is that
     * its combining marks survive, which is the safe direction — a preserved accent can only make a
     * query match less, never match the wrong place.
     */
    internal fun isLatinishBase(codePoint: Int): Boolean = when (codePoint) {
        in 0x0041..0x007A, in 0x00C0..0x02AF -> true
        in 0x0370..0x03FF, in 0x1F00..0x1FFF -> true
        in 0x0400..0x052F, in 0x2DE0..0x2DFF, in 0xA640..0xA69F -> true
        else -> false
    }

    /**
     * Whether this character is a combining mark (`Mn`, `Mc` or `Me`).
     *
     * The table is the union of the three categories, written to be non-overlapping: where `Mc`
     * ranges sit inside an `Mn` range they are omitted rather than repeated.
     *
     * This does **not** use `Character.getType`, which would be shorter and would classify the whole
     * Unicode range. Matching the Swift and Rust tables exactly matters more than brevity here: the
     * tables differ from the category in places, and a divergence would show up as an
     * implementation that passes its own tests and fails the shared vectors.
     */
    internal fun isCombiningMark(codePoint: Int): Boolean = when (codePoint) {
        // Mn — nonspacing marks
        in 0x0300..0x036F, in 0x0483..0x0489, in 0x0591..0x05BD, 0x05BF,
        in 0x05C1..0x05C2, in 0x05C4..0x05C5, 0x05C7, in 0x0610..0x061A,
        in 0x064B..0x065F, 0x0670, in 0x06D6..0x06DC, in 0x06DF..0x06E4,
        in 0x06E7..0x06E8, in 0x06EA..0x06ED, 0x0711, in 0x0730..0x074A,
        in 0x07A6..0x07B0, in 0x07EB..0x07F3, in 0x0816..0x0819, in 0x081B..0x0823,
        in 0x0825..0x0827, in 0x0829..0x082D, in 0x0859..0x085B, in 0x08D3..0x08E1,
        in 0x08E3..0x0903, in 0x093A..0x093C, in 0x093E..0x094F,
        in 0x0951..0x0957, in 0x0962..0x0963 -> true
        // Mn — the Japanese kana marks this whole implementation exists to preserve
        in 0x3099..0x309A, in 0x309B..0x309C -> true
        // Me — enclosing marks
        in 0x20D0..0x20F0 -> true
        // Mc — spacing marks, only those NOT already covered above
        in 0x0982..0x0983, in 0x09BF..0x09C0, in 0x09C7..0x09C8, in 0x09CB..0x09CC,
        0x0A03, in 0x0A3E..0x0A40, 0x0A83, in 0x0ABE..0x0AC0, 0x0AC9,
        in 0x0ACB..0x0ACC, 0x0B3C, in 0x0B3E..0x0B40, 0x0B47, in 0x0B4B..0x0B4C,
        0x0B57, in 0x0BBE..0x0BBF, in 0x0BC1..0x0BC2, in 0x0BC6..0x0BC8,
        in 0x0BCA..0xBCC, in 0x0C01..0x0C03, in 0x0C41..0x0C44, in 0x0C82..0x0C83,
        0x0CBE, in 0x0CC0..0x0CC1, in 0x0CC3..0x0CC4, in 0x0CC7..0x0CC8,
        in 0x0CCA..0x0CCB, in 0x0D02..0x0D03, in 0x0D3F..0x0D40, in 0x0D46..0x0D48,
        in 0x0D4A..0x0D4C, in 0x0D82..0x0D83 -> true
        else -> false
    }

    /**
     * Step 2: strip combining marks on Latinish bases only.
     *
     * Iterates **code points**, never Kotlin `Char`. A `Char` is a UTF-16 code unit, so iterating it
     * splits an astral scalar into two surrogates and gives the mark test a nonsense value to judge.
     * More importantly the decision looks at the *preceding* code point, because after NFKD a kana
     * dakuten sits after a base that is not Latinish. Stripping on the mark alone is what turned
     * `ガザ` into `カサ`.
     */
    private fun stripLatinishMarks(text: String): String {
        val out = StringBuilder(text.length)
        var baseIsLatinish = false
        text.codePoints().forEach { codePoint ->
            when {
                isCombiningMark(codePoint) ->
                    // Preserved when the base is not Latinish: a Japanese dakuten is the difference
                    // between two kana.
                    if (!baseIsLatinish) out.appendCodePoint(codePoint)
                else -> {
                    baseIsLatinish = isLatinishBase(codePoint)
                    out.appendCodePoint(codePoint)
                }
            }
        }
        return out.toString()
    }

    /**
     * Steps 6–8: separator handling.
     *
     * Code points again, for the same reason: a combining mark that survived step 2 is part of its
     * base as far as word-internality is concerned.
     */
    private fun collapseSeparators(text: String): String {
        val out = StringBuilder(text.length)
        var pendingSpace = false
        text.codePoints().forEach { codePoint ->
            // Combining marks count as word-internal. Any mark still present here survived step 2, so
            // it is load-bearing; treating `Mn` as a separator turned `ガザ` into `カ サ`.
            val isWord = Character.isLetterOrDigit(codePoint) || isCombiningMark(codePoint)
            if (isWord) {
                if (pendingSpace && out.isNotEmpty()) out.append(' ')
                out.appendCodePoint(codePoint)
                pendingSpace = false
            } else {
                pendingSpace = true
            }
        }
        return out.toString()
    }
}