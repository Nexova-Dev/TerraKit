package com.terrakit.core

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

/**
 * Normalization assertions that need no fixture, so they run on **every** target including Android.
 *
 * The fixture-driven suite in `jvmTest` is the one that proves agreement with the other
 * implementations. This one proves the properties hold at all, on a platform where reading a file
 * from the repository is not possible — an Android unit test has no repository, so a suite that only
 * exists there would silently never run on the platform where shipping is most likely.
 */
class NormalizationSpecTest {

    /** The examples `core/spec/normalization.md` §8 names explicitly. */
    @Test
    fun specExamples() {
        val expected = listOf(
            "Istanbul" to "istanbul",
            "İstanbul" to "istanbul",
            "ISTANBUL" to "istanbul",
            "Atatürk" to "ataturk",
            "Ataturk" to "ataturk",
            "Muratpaşa" to "muratpasa",
            "Türkiye" to "turkiye",
            "Straße" to "strasse",
            "Ağrı" to "agri",
            "İzmir" to "izmir",
        )
        for ((input, want) in expected) {
            assertEquals(want, Normalization.normalize(input), "normalize(${input.debugLiteral()})")
        }
    }

    /**
     * The defect that made Japanese search return nothing: a categorical mark strip rewrote these
     * into different names and collided カ with ガ.
     */
    @Test
    fun japaneseDakutenSurvives() {
        for (word in listOf("ガザ", "バ", "パ", "ヴ", "イスタンブール", "東京都")) {
            assertEquals(word, Normalization.normalize(word), word.debugLiteral())
        }
        assertNotEquals(Normalization.normalize("カ"), Normalization.normalize("ガ"))
        assertNotEquals(Normalization.normalize("パ"), Normalization.normalize("ハ"))
        assertFalse(Normalization.normalize("ガザ").contains(' '))
    }

    /** NFKD leaves Hangul as decomposed jamo; step 9 must put it back. */
    @Test
    fun hangulRecomposes() {
        for (word in listOf("서울", "부산")) {
            assertEquals(word, Normalization.normalize(word), word.debugLiteral())
        }
    }

    /** §6: these are different rules, and conflating them breaks one or merges two distinct keys. */
    @Test
    fun apostropheRemovedButHyphenSpaced() {
        assertEquals("dangelo", Normalization.normalize("D'Angelo"))
        assertEquals("dangelo", Normalization.normalize("D’Angelo"))
        assertEquals("saint denis", Normalization.normalize("Saint-Denis"))
    }

    @Test
    fun postalNormalizationIsStricter() {
        assertEquals("sw1a1aa", Normalization.postalNormalize("SW1A 1AA"))
        assertEquals("sw1a1aa", Normalization.postalNormalize("SW1A1AA"))
        assertEquals("sw1a1aa", Normalization.postalNormalize("SW1A-1AA"))
        assertEquals("07160", Normalization.postalNormalize("07160"))
        assertEquals("07000", Normalization.postalNormalize("  07000  "))
    }

    @Test
    fun searchKeyStripsAllSeparators() {
        assertEquals("ataturkcaddesi", Normalization.searchKeyUnaccented("Atatürk Caddesi"))
        assertEquals("newyork", Normalization.searchKeyUnaccented("New York"))
        assertEquals("東京都", Normalization.searchKeyUnaccented("東京都"))
    }

    /**
     * A visible soft hyphen must not survive: it is invisible and commonly pasted in from word
     * processors and PDFs, so a key carrying one matches nothing at all.
     */
    @Test
    fun softHyphenIsRemoved() {
        assertEquals("antalya", Normalization.normalize("antal­ya"))
        assertEquals("antalya", Normalization.normalize("An­talya"))
    }

    /** NFKD is required, not merely NFD: these have only compatibility decompositions. */
    @Test
    fun compatibilityDecompositionsApply() {
        assertEquals("final", Normalization.normalize("ﬁnal"))
        assertEquals("1", Normalization.normalize("①"))
        assertEquals("xii", Normalization.normalize("Ⅻ"))
        assertEquals("123", Normalization.normalize("１２３"))
    }

    @Test
    fun emptyInputIsEmpty() {
        assertEquals("", Normalization.normalize(""))
        assertEquals("", Normalization.normalize("   "))
        assertEquals("", Normalization.normalize("---"))
    }

    /** Output must be a fixed point: normalizing twice changes nothing. */
    @Test
    fun normalizationIsIdempotent() {
        for (word in listOf("İstanbul", "Straße", "ガザ", "서울", "TŪRKIYE", "Ærø", "Đà Nẵng")) {
            val once = Normalization.normalize(word)
            assertEquals(once, Normalization.normalize(once), "$word is not idempotent")
        }
    }

    /** No Latinish combining mark may survive. Narrower than "no marks at all", which would be wrong. */
    @Test
    fun noLatinishCombiningMarksSurvive() {
        val samples = listOf("İstanbul", "Ångström", "Çanakkale", "Ελλάδα", "Москва", "Ẓẓ")
        for (sample in samples) {
            val result = Normalization.normalize(sample)
            var baseIsLatinish = false
            result.codePoints().forEach { codePoint ->
                val isMark = Normalization.isCombiningMark(codePoint)
                assertFalse(
                    isMark && baseIsLatinish,
                    "kept a Latinish combining mark: ${result.debugLiteral()}"
                )
                if (!isMark) baseIsLatinish = Normalization.isLatinishBase(codePoint)
            }
        }
    }

    @Test
    fun turkishDottedAndDotlessIFoldTogether() {
        // The headline case in §8: a user typing either spelling must reach the same place.
        assertEquals(
            Normalization.normalize("Istanbul"),
            Normalization.normalize("İstanbul")
        )
        assertEquals(
            Normalization.normalize("Izmir"),
            Normalization.normalize("İzmir")
        )
    }

    @Test
    fun searchKeyMinimumLengthIsThree() {
        // Asserted because the FTS5 trigram tokenizer's floor is a documented API behaviour, and a
        // change to it would silently change which queries are searchable.
        assertEquals(3, Normalization.SEARCH_KEY_MINIMUM_LENGTH)
    }
}

/** A readable rendering for assertion messages, so a failure names the code points not the glyphs. */
internal fun String.debugLiteral(): String {
    val body = codePoints().boxed().map { codePoint ->
        if (codePoint in 0x20..0x7E && Character.isLetterOrDigit(codePoint)) {
            codePoint.toChar().toString()
        } else {
            "\\u%04X".format(codePoint)
        }
    }.collect(java.util.stream.Collectors.joining(""))
    return "\"$body\""
}

internal fun assertTrueSoft(condition: Boolean, message: String) = assertTrue(condition, message)