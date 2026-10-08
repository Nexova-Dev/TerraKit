package com.terrakit.core

/**
 * One package database, wrapped.
 *
 * Exists to check the SQLite floor, cache the capability probe and give the query engine a single
 * place to ask "run this" — and for nothing else. Every statement in the SDK is visible here, which
 * is the point: a query engine whose SQL is spread across a builder is a query engine whose plans
 * cannot be compared across SDKs, which is what the conformance suite exists to do.
 */
internal class Database(
    val path: String,
    val countryCode: String,
) : AutoCloseable {

    private val handle: SqlHandle

    init {
        val opened = try {
            openSqlite(path, countryCode)
        } catch (error: TerraError) {
            throw error
        } catch (error: Throwable) {
            throw TerraError.DatabaseUnreadable("${path.substringAfterLast('/')}: ${error.message}")
        }
        handle = opened

        // The trigram tokenizer arrived in SQLite 3.34. Checked rather than assumed: an SDK that
        // opens a package on an older engine fails on the first search with an FTS5 error that says
        // nothing about versions.
        if (!isVersionSupported(handle.sqliteVersion)) {
            handle.close()
            throw TerraError.UnsupportedSqliteVersion(handle.sqliteVersion, "3.34")
        }
    }

    fun <T> query(
        sql: String,
        bindings: List<SqlValue> = emptyList(),
        limit: Int = QueryLimits.MAXIMUM_ENTITIES,
        map: (SqlRow) -> T,
    ): List<T> = handle.select(sql, bindings, limit.coerceAtLeast(0), map)

    /** The first row, or `null`. */
    fun <T> first(
        sql: String,
        bindings: List<SqlValue> = emptyList(),
        map: (SqlRow) -> T,
    ): T? = handle.select(sql, bindings, 1, map).firstOrNull()

    fun capabilities(): Capabilities = handle.capabilities()

    /** The engine version behind this connection, e.g. `3.41.2`. */
    val sqliteVersion: String get() = handle.sqliteVersion

    override fun close() = handle.close()

    companion object {
        /**
         * The trigram tokenizer the search index depends on arrived in 3.34.
         *
         * Compared numerically rather than as strings, because `"3.9" < "3.34"` is true and
         * comparing versions textually gets the ordering backwards for every two-digit minor.
         */
        fun isVersionSupported(version: String): Boolean {
            val parts = version.split('.').mapNotNull { it.takeWhile(Char::isDigit).toIntOrNull() }
            if (parts.size < 2) return false
            return when {
                parts[0] > 3 -> true
                parts[0] < 3 -> false
                else -> parts[1] >= 34
            }
        }
    }
}