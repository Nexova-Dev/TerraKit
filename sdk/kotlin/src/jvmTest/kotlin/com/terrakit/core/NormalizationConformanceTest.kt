package com.terrakit.core

import java.io.File
import kotlin.test.Test
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * Cross-language normalization conformance.
 *
 * `tests/fixtures/normalization.json` is the same oracle the Python pipeline, the Rust CLI and the
 * Swift SDK are held to. Reading it here is what makes the Kotlin normalizer *verifiable* rather
 * than merely present: the SDK and the CLI must agree on which places a query matches, and the only
 * way to know they agree is to check them against one set of vectors.
 *
 * A failing vector here is a real defect, not a fixture to be adjusted. See `CONTRIBUTING.md` §5.1.
 *
 * The repository root arrives in `TERRAKIT_REPO_ROOT`, set by the Gradle build from `rootDir`. It is
 * not derived from the working directory, because Gradle does not guarantee one and a conformance
 * suite that cannot find its oracle fails in a way that looks like a pass on CI and a failure
 * locally.
 */
class NormalizationConformanceTest {

    private data class Vector(val n: Int, val input: String, val expected: String, val covers: String?)

    private val vectors: List<Vector> by lazy { loadVectors() }

    @Test
    fun everyFixtureVectorPasses() {
        assertTrue(vectors.isNotEmpty(), "the fixture parsed to zero vectors")
        val failures = vectors.mapNotNull { vector ->
            val actual = Normalization.normalize(vector.input)
            if (actual == vector.expected) null else {
                buildString {
                    appendLine("#${vector.n} ${vector.covers ?: ""}")
                    appendLine("    input:    ${vector.input.debugLiteral()}")
                    appendLine("    expected: ${vector.expected.debugLiteral()}")
                    appendLine("    actual:   ${actual.debugLiteral()}")
                }
            }
        }
        if (failures.isNotEmpty()) {
            fail(
                "${failures.size}/${vectors.size} vectors failed:\n\n" +
                    failures.joinToString("\n") +
                    "\n\nDo not edit the fixture to make these pass. See CONTRIBUTING.md section 5.1."
            )
        }
    }

    @Test
    fun outputIsIdempotentForEveryVector() {
        for (vector in vectors) {
            val once = Normalization.normalize(vector.input)
            assertTrue(
                once == Normalization.normalize(once),
                "vector #${vector.n} is not idempotent: ${vector.input.debugLiteral()}"
            )
        }
    }

    /**
     * The fixture records that no Latinish combining mark may survive the pipeline. Checked here as
     * well so a normalizer that passes every vector but keeps a mark is caught — the vectors cannot
     * express that property on their own.
     */
    @Test
    fun noLatinishCombiningMarksSurvive() {
        for (vector in vectors) {
            val result = Normalization.normalize(vector.input)
            var baseIsLatinish = false
            result.codePoints().forEach { codePoint ->
                val isMark = Normalization.isCombiningMark(codePoint)
                assertTrue(
                    !(isMark && baseIsLatinish),
                    "vector #${vector.n} kept a Latinish combining mark: ${result.debugLiteral()}"
                )
                if (!isMark) baseIsLatinish = Normalization.isLatinishBase(codePoint)
            }
        }
    }

    // MARK: - Fixture reading
    //
    // Hand-rolled rather than pulled from a JSON library, because ADR-007's zero-dependency rule
    // covers the SDK and adding a parser to satisfy a test would be a poor trade. This reads exactly
    // the fixture's schema: a `vectors` array of objects with `n`, `input`, `expected`, `covers`.
    //
    // It **fails loudly** on anything else rather than skipping what it cannot parse. A tolerant
    // reader that quietly skipped a vector would report coverage it does not have — which is the
    // same failure mode as a test that skips when its fixtures are missing.

    private fun loadVectors(): List<Vector> {
        val root = System.getenv("TERRAKIT_REPO_ROOT")
            ?: fail(
                """
                TERRAKIT_REPO_ROOT is not set, so the oracle cannot be found.

                Run through Gradle, which sets it, or set it by hand:
                  TERRAKIT_REPO_ROOT=$(git rev-parse --show-toplevel) ./gradlew jvmTest
                """.trimIndent()
            )
        val fixture = File(root, "tests/fixtures/normalization.json")
        if (!fixture.isFile) fail("${fixture.path} does not exist")
        val text = fixture.readText()

        val vectors = mutableListOf<Vector>()
        // One object at a time, then the three fields inside it.
        for (objectMatch in OBJECTS.findAll(text)) {
            val body = objectMatch.groupValues[1]
            // Failing rather than skipping. A tolerant reader that quietly dropped an object it
            // could not parse would report coverage it does not have, which is the same failure
            // mode as a suite that skips when its fixtures are missing -- and the fixture is the
            // oracle, so a silently shortened oracle is the worst kind of bug in this file.
            val n = intField(body, "n")
                ?: fail("a vector in the fixture has no 'n' field; the schema changed:\n$body")
            val input = stringField(body, "input")
                ?: fail("vector #$n has no 'input' field; the schema changed:\n$body")
            val expected = stringField(body, "expected")
                ?: fail("vector #$n has no 'expected' field; the fixture is malformed")
            vectors += Vector(n, input, expected, stringField(body, "covers"))
        }
        return vectors.sortedBy { it.n }
    }

    private val OBJECTS = Regex("""\{([^{}]*\n[^{}]*)\}""")

    private fun intField(body: String, name: String): Int? =
        Regex(""""$name"\s*:\s*(-?\d+)""").find(body)?.groupValues?.get(1)?.toInt()

    /** Read a JSON string field, decoding the escapes the fixture actually uses. */
    private fun stringField(body: String, name: String): String? {
        val match = Regex(""""$name"\s*:\s*"((?:[^"\\]|\\.)*)"""").find(body) ?: return null
        return unescape(match.groupValues[1])
    }

    private fun unescape(raw: String): String {
        val out = StringBuilder(raw.length)
        var i = 0
        while (i < raw.length) {
            val c = raw[i]
            if (c != '\\') {
                out.append(c)
                i++
                continue
            }
            when (val escape = raw.getOrNull(i + 1)) {
                'u' -> {
                    out.append(raw.substring(i + 2, i + 6).toInt(16).toChar())
                    i += 6
                }
                'n' -> { out.append('\n'); i += 2 }
                't' -> { out.append('\t'); i += 2 }
                'r' -> { out.append('\r'); i += 2 }
                '"' -> { out.append('"'); i += 2 }
                '\\' -> { out.append('\\'); i += 2 }
                '/' -> { out.append('/'); i += 2 }
                else -> fail("unhandled escape '\\$escape' in the fixture")
            }
        }
        return out.toString()
    }
}