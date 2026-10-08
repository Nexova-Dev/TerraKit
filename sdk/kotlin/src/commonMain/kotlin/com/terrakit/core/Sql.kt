package com.terrakit.core

/**
 * The statement-execution boundary.
 *
 * The SDK's whole platform surface is the three interfaces below and one [openSqlite] factory. That
 * is the whole reason a multiplatform build is needed at all: normalization, models, ranking and
 * every query live once in common code, and the only per-platform code is "run this SQL and give me
 * rows".
 *
 * Deliberately no query builder, no row mapper and no connection pool abstraction beyond
 * [SqlHandle]. A builder here would be a second data-access layer that has to be kept in step with
 * the SQL in the query engine, and [ADR-003] specifies raw prepared statements precisely so that
 * neither the Rust nor the Swift nor the Kotlin implementation acquires one.
 */
internal sealed interface SqlValue {
    data class Text(val value: String) : SqlValue
    data class Integer(val value: Long) : SqlValue
    data class Real(val value: Double) : SqlValue
    data object Null : SqlValue

    companion object {
        fun text(value: String?): SqlValue = value?.let { Text(it) } ?: Null
        fun integer(value: Int?): SqlValue = value?.let { Integer(it.toLong()) } ?: Null
        fun integer(value: Long?): SqlValue = value?.let { Integer(it) } ?: Null
        fun real(value: Double?): SqlValue = value?.let { Real(it) } ?: Null
    }
}

/** One row, read by zero-based column index. */
internal interface SqlRow {
    fun text(index: Int): String?
    fun int(index: Int): Long?
    fun double(index: Int): Double?
    fun isNull(index: Int): Boolean
}

/** What this SQLite build supports. Probed, never assumed — see [SqlHandle.capabilities]. */
internal data class Capabilities(val fts5: Boolean, val rtree: Boolean)

/**
 * One read-only connection to one package database.
 *
 * **Read-only, enforced by the open mode rather than by discipline.** A package file is an artifact,
 * and an SDK that can write to it can corrupt the thing it was handed.
 */
internal interface SqlHandle : AutoCloseable {

    /** The engine version, e.g. `3.41.2`. Reported so the floor can be checked in common code. */
    val sqliteVersion: String

    /**
     * Run a query and map each row, stopping after `limit`.
     *
     * `limit` is enforced by the implementation rather than trusted to the SQL, because the SQL's own
     * `LIMIT` is a statement of intent and this is the guarantee. [SDK_SPEC §12](../../SDK_SPEC.md)
     * caps every list-returning method, and a cap that lives only in a query string is one typo away
     * from not existing.
     *
     * **The [SqlRow] is valid only for the duration of [map].** It wraps a live cursor, and the
     * implementation closes that cursor as soon as [map] returns — returning the row itself is the
     * natural thing to write when all you need is one record, and it fails later with
     * "stmt pointer is closed", at a call site nowhere near the query. Copy the values out instead.
     */
    fun <T> select(sql: String, bindings: List<SqlValue>, limit: Int, map: (SqlRow) -> T): List<T>

    /**
     * Whether this build has FTS5 with the trigram tokenizer and R\*Tree.
     *
     * Probed rather than compiled-for: both are optional SQLite features, and a diagnostics screen
     * needs to report what is actually available on the device it is running on.
     *
     * **Probed once and cached.** A probe that creates a temporary table and creates it again on the
     * next call fails the second time, so an uncached probe answers `true` once and `false` forever
     * after. The symptom is that the first fuzzy search in a session works and every later one
     * silently returns nothing — which looks like a missing index rather than a bug in the check.
     */
    fun capabilities(): Capabilities
}

/**
 * Open a package database read-only.
 *
 * The JVM implementation binds to `libsqlite3` through JDBC ([ADR-008](../docs/adr/ADR-008-kotlin-sqlite-binding.md));
 * the Android one uses `android.database.sqlite`, which is in the platform.
 *
 * Takes a **path**, not an asset stream, on both. On Android a package shipped inside the APK has to
 * be copied out before SQLite can open it — a database file inside a compressed asset is not
 * addressable — and doing that is the platform's business, not the query engine's. See
 * `AssetInstaller` on Android for the copy.
 */
internal expect fun openSqlite(path: String, countryCode: String): SqlHandle

/** Hard limits, `SDK_SPEC.md` §12. Enforced, not documented-and-hoped-for. */
internal object QueryLimits {
    /** Ceiling on any list-returning call. */
    const val MAXIMUM_ENTITIES = 1000

    /** Autocomplete runs on every keystroke, so its cap is much lower. */
    const val AUTOCOMPLETE = 50

    /**
     * Ceiling on candidates pulled from the R\*Tree before distance is computed.
     *
     * The R\*Tree is a candidate generator, never the answer
     * ([`ADR-005`](../../docs/adr/ADR-005-postgis-sqlite-rtree.md)): a bounding-box prefilter
     * degrades at high latitudes, exactly where data is densest. So more candidates are taken and
     * then ordered by haversine in SDK code.
     */
    const val NEAREST_CANDIDATES = 400
}

/**
 * Render a bound value as a SQL literal.
 *
 * **Used only by the Android binding**, and the reason is a platform limitation rather than a
 * preference. Android's public API for a query that returns rows is
 * `SQLiteDatabase.rawQuery(sql, String[])`, and AOSP binds *every* argument as text. `LIMIT ?2`
 * happens to survive that, but `r.min_lon <= ?1` against a REAL column does not: SQLite orders REAL
 * before TEXT, so a text bound makes the comparison false for every row and `nearest` silently
 * returns nothing. `SQLiteProgram.executeQueryCursor()` would give typed binds, but it is not in
 * `android.jar` — `javap` on API 34 shows only the four `bind*` methods.
 *
 * So on Android a bound value is rendered and placed in the SQL. This is safe here because every
 * value the SDK binds is either package-derived text or an SDK-computed number — no user string ever
 * reaches a binding, since queries take a *normalized key*, not raw input. The escaping below is
 * still complete, so the safety does not rest on that alone.
 *
 * The JVM binding does not use this; it binds properly. The two must agree on results, and
 * `AndroidLiteralRenderingMatchesPreparedStatements` proves they do by running the same queries both
 * ways against the same package and comparing the rows.
 */
internal object SqlLiteral {

    /**
     * Replace every `?N` placeholder in [sql] with the literal for `bindings[N - 1]`.
     *
     * Only `?N` is recognised, never a bare `?`: the SDK's SQL always numbers its placeholders, and
     * accepting the bare form would mean reimplementing SQLite's "next free number" rule, which is
     * exactly the ambiguity that made the CLI's `--country` plus `--type` combination bind four
     * parameters against three placeholders.
     */
    fun render(sql: String, bindings: List<SqlValue>): String {
        val out = StringBuilder(sql.length + 32)
        var index = 0
        while (index < sql.length) {
            val c = sql[index]
            if (c != '?') {
                out.append(c)
                index++
                continue
            }
            var end = index + 1
            while (end < sql.length && sql[end].isDigit()) end++
            if (end == index + 1) {
                // A bare `?`. Left as-is: it is not this renderer's job to guess, and a query that
                // reaches here would have failed loudly on the JVM.
                out.append(c)
                index++
                continue
            }
            val slot = sql.substring(index + 1, end).toInt()
            val value = bindings.getOrNull(slot - 1)
            out.append(if (value == null) "NULL" else renderValue(value))
            index = end
        }
        return out.toString()
    }

    fun renderValue(value: SqlValue): String = when (value) {
        SqlValue.Null -> "NULL"
        // `%.17g` round-trips an IEEE double exactly and cannot produce a quote, a backslash or any
        // other SQL metacharacter, so no escaping is possible or needed. A non-finite value would
        // render as `NaN` or `Infinity`, which SQLite rejects; callers never produce one because
        // coordinates are range-checked at construction.
        is SqlValue.Real -> formatReal(value.value)
        is SqlValue.Integer -> value.value.toString()
        is SqlValue.Text -> quote(value.value)
    }

    private fun formatReal(value: Double): String {
        // A non-finite value would render as `NaN` or `Infinity`, which SQLite rejects. Callers never
        // produce one because coordinates are range-checked at construction, so reaching this is a
        // programming error and is reported as one rather than silently becoming NULL.
        require(value.isFinite()) { "non-finite coordinate: $value" }
        return formatDouble(value)
    }

    private fun formatDouble(value: Double): String {
        val text = value.toString()
        // Kotlin renders a whole double as "1.0" and a small one in scientific notation such as
        // "1.0E-5". SQLite accepts both, so the only adjustment needed is ensuring an exponent is
        // preceded by a digit with a decimal point, which `toString` already guarantees.
        return if (text.contains('E') && !text.contains('.')) {
            text.replaceFirst("E", ".0E")
        } else {
            text
        }
    }

    /**
     * Quote a string literal.
     *
     * Doubling the single quote is SQLite's own escape and it is complete for this purpose. A NUL byte
     * is emitted as `char(0)` **concatenated between quotes** — `'a' || char(0) || 'b'`, not
     * `'a char(0) b'`, which would be the literal text "char(0)". A raw NUL inside a SQL string
     * truncates it in some parsers, and a normalized key can legitimately contain one if the source
     * text did.
     */
    fun quote(value: String): String {
        val out = StringBuilder(value.length + 2)
        out.append('\'')
        for (c in value) {
            when {
                c == '\'' -> out.append("''")
                // Close the literal, concatenate a real NUL, reopen it.
                c == '\u0000' -> out.append("' || char(0) || '")
                else -> out.append(c)
            }
        }
        out.append('\'')
        return out.toString()
    }
}
