package com.terrakit.core

import java.io.File
import kotlin.math.cos
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * The Android binding renders bound values into the SQL instead of binding them, because Android has
 * no public API for typed binds on a query that returns rows.
 *
 * [SqlLiteral] documents why, and this file is the part that matters more than the explanation: it
 * proves the two mechanisms return **the same rows**. A difference in mechanism is acceptable under
 * [SDK_SPEC §1](../../SDK_SPEC.md) only because the contract is about results, and a claim that the
 * two agree is worth nothing unless something checks it.
 */
class SqlLiteralTest {

    @Test
    fun textIsQuotedAndEmbeddedQuotesDoubled() {
        assertEquals("'antalya'", SqlLiteral.quote("antalya"))
        assertEquals("''", SqlLiteral.quote(""))
        // Doubling is SQLite's own escape and it is complete for this purpose.
        assertEquals("'O''Brien'", SqlLiteral.quote("O'Brien"))
        assertEquals("'a''b''c'", SqlLiteral.quote("a'b'c"))
    }

    @Test
    fun nullIsRenderedAsNull() {
        assertEquals("NULL", SqlLiteral.renderValue(SqlValue.Null))
        assertEquals("SELECT NULL IS NULL", SqlLiteral.render("SELECT ?1 IS NULL", listOf(SqlValue.Null)))
    }

    @Test
    fun numbersKeepTheirType() {
        assertEquals("400", SqlLiteral.renderValue(SqlValue.Integer(400)))
        assertEquals("42", SqlLiteral.renderValue(SqlValue.integer(42)))
        // A REAL must not become an integer literal: the R*Tree columns are doubles and a truncated
        // coordinate changes which rows the prefilter admits.
        assertEquals("36.9", SqlLiteral.renderValue(SqlValue.Real(36.9)))
        assertEquals("30.0", SqlLiteral.renderValue(SqlValue.Real(30.0)))
        assertTrue(
            SqlLiteral.renderValue(SqlValue.Real(1.0E-5)).contains("E"),
            "a small coordinate must keep its exponent rather than becoming 0.0"
        )
    }

    @Test
    fun aNulByteBecomesACharLiteral() {
        // A raw NUL inside a SQL string is truncated by some parsers, and a normalized key can carry
        // one if the source text did.
        assertEquals("'a' || char(0) || 'b'", SqlLiteral.quote("a\u0000b"))
    }

    @Test
    fun placeholdersAreReplacedInOrder() {
        val sql = "SELECT * FROM places WHERE name_norm = ?1 AND country_iso2 = ?2 LIMIT ?3"
        val rendered = SqlLiteral.render(
            sql,
            listOf(
                SqlValue.text("antalya"),
                SqlValue.text("TR"),
                SqlValue.integer(20)
            )
        )
        assertEquals("SELECT * FROM places WHERE name_norm = 'antalya' AND country_iso2 = 'TR' LIMIT 20", rendered)
    }

    @Test
    fun multiDigitPlaceholderNumbersAreNotConfusedWithDigits() {
        val sql = "SELECT ?10, ?1"
        val bindings = (1..10).map { SqlValue.integer(it) }
        assertEquals("SELECT 10, 1", SqlLiteral.render(sql, bindings))
    }

    @Test
    fun aBareQuestionMarkIsLeftAloneRatherThanGuessed() {
        // Accepting the bare form would mean reimplementing SQLite's "next free number" rule, which is
        // the ambiguity that made the CLI bind four parameters against three placeholders.
        val rendered = SqlLiteral.render("SELECT '?', ?1", listOf(SqlValue.integer(1)))
        assertTrue(rendered.contains("'?'"), "a question mark inside a literal must survive")
        assertTrue(rendered.endsWith("1"))
    }

    @Test
    fun aMissingBindingBecomesNullRatherThanThrowing() {
        // A short binding list is a bug in the caller. Rendering NULL surfaces it as a query that
        // returns no rows, which the conformance suite would report as a divergence; throwing here
        // would surface it at the query site instead, which is better but would mean guessing which
        // placeholder was meant.
        assertEquals("SELECT NULL", SqlLiteral.render("SELECT ?2", listOf(SqlValue.integer(1))))
    }

    // MARK: - The equivalence that matters

    /**
     * The same query, prepared and rendered, over the same package, returning the same rows.
     *
     * The nearest query is the one that would break: its bounding-box bounds are REALs compared
     * against REAL columns, which is exactly where Android's text binding fails silently. Running it
     * both ways over real data is the only check that the renderer is faithful for the case that
     * matters, rather than merely faithful for the examples above.
     */
    @Test
    fun androidLiteralRenderingMatchesPreparedStatements() {
        val packagesDirectory = System.getenv("TERRAKIT_CONFORMANCE_PACKAGES")
            ?: fail(
                """
                TERRAKIT_CONFORMANCE_PACKAGES is not set.

                  TERRAKIT_CONFORMANCE_PACKAGES=<datasets> ./gradlew crossSdkConformance
                """.trimIndent()
            )
        Terra.configure(File(packagesDirectory))

        val pkg = Terra.shared().requirePackage("TR")
        val handle = openSqlite(File(pkg.directory, "data.sqlite").path, "TR")
        try {
            val centre = Coordinate(36.8969, 30.7133)
            val latitudeDelta = 50.0 / 111.32
            val longitudeDelta = 50.0 / (111.32 * maxOf(cos(Math.toRadians(centre.latitude)), 0.01))
            val sql = """
                SELECT p.id
                FROM places_rtree r JOIN places p ON p.rowid_int = r.id_rowid
                WHERE r.min_lon <= ?1 AND r.max_lon >= ?2
                  AND r.min_lat <= ?3 AND r.max_lat >= ?4
                  AND p.latitude IS NOT NULL AND p.longitude IS NOT NULL
                LIMIT ?5
            """
            val bindings = listOf(
                SqlValue.Real(centre.longitude + longitudeDelta),
                SqlValue.Real(centre.longitude - longitudeDelta),
                SqlValue.Real(centre.latitude + latitudeDelta),
                SqlValue.Real(centre.latitude - latitudeDelta),
                SqlValue.Integer(QueryLimits.NEAREST_CANDIDATES.toLong())
            )

            val prepared = handle.select(sql, bindings, QueryLimits.NEAREST_CANDIDATES) { it.text(0) }
            val rendered = handle.select(
                SqlLiteral.render(sql, bindings),
                emptyList(),
                QueryLimits.NEAREST_CANDIDATES
            ) { it.text(0) }

            assertTrue(prepared.isNotEmpty(), "the bounding box matched nothing, so nothing was compared")
            assertEquals(prepared, rendered, "rendering changed the result set")
        } finally {
            handle.close()
        }
    }

    /**
     * Every battery query renders to SQL the engine accepts.
     *
     * A syntactic break in the renderer would otherwise be discovered on a device, where the test
     * suite cannot reach: this runs the rendered form of the SDK's own statements against the same
     * packages the prepared form is verified on.
     */
    @Test
    fun everySearchStatementRendersToValidSql() {
        val packagesDirectory = System.getenv("TERRAKIT_CONFORMANCE_PACKAGES")
            ?: fail("TERRAKIT_CONFORMANCE_PACKAGES is not set")
        Terra.configure(File(packagesDirectory))

        val pkg = Terra.shared().requirePackage("TR")
        val handle = openSqlite(File(pkg.directory, "data.sqlite").path, "TR")
        try {
            val key = Normalization.normalize("Antalya")
            val statements = listOf(
                Triple(
                    "SELECT ${Queries.PLACE_COLUMNS} FROM places p WHERE p.name_norm = ?1 LIMIT ?2",
                    listOf(SqlValue.text(key), SqlValue.integer(10)),
                    "exact"
                ),
                Triple(
                    "SELECT ${Queries.PLACE_COLUMNS} FROM places p WHERE p.name_norm LIKE ?1 ESCAPE '\\' LIMIT ?2",
                    listOf(SqlValue.text(Queries.escapeLike(key) + "%"), SqlValue.integer(10)),
                    "prefix"
                ),
                Triple(
                    """
                    SELECT ${Queries.PLACE_COLUMNS}
                    FROM place_closure c JOIN places p ON p.id = c.ancestor_id
                    WHERE c.descendant_id = ?1 ORDER BY c.depth DESC
                    """,
                    listOf(SqlValue.text("country:TR")),
                    "ancestors"
                ),
            )
            for ((sql, bindings, label) in statements) {
                val prepared = handle.select(sql, bindings, 32) { Queries.place(it).id }
                val rendered = handle.select(SqlLiteral.render(sql, bindings), emptyList(), 32) {
                    Queries.place(it).id
                }
                assertEquals(prepared, rendered, "$label changed when rendered")
            }
        } finally {
            handle.close()
        }
    }
}