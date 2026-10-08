package com.terrakit.core

import java.net.URLEncoder
import java.sql.DriverManager
import java.sql.ResultSet

/**
 * The JVM's SQLite binding, through JDBC.
 *
 * [ADR-008](../../../docs/adr/ADR-008-kotlin-sqlite-binding.md) is the decision this file exists to
 * implement: `sqlite-jdbc` is the only dependency the SDK carries, and it occupies the position
 * `libsqlite3` occupies in the Swift SDK — the binding to the SQLite C library, and nothing above it.
 * There is no query builder, no row mapper and no serializer anywhere in this file or the ones it
 * delegates to.
 */
internal actual fun openSqlite(path: String, countryCode: String): SqlHandle =
    JdbcHandle(path, countryCode)

private class JdbcHandle(path: String, private val countryCode: String) : SqlHandle {

    /**
     * The connection, guarded by [lock].
     *
     * One connection behind a lock rather than a pool: [SDK_SPEC.md §11](../../SDK_SPEC.md) allows
     * one per thread or a single one behind a lock, SQLite in serialized mode is fast enough at these
     * query shapes that pooling would be premature, and the connection count staying at one is what
     * `sqlite3_threadsafe` supports cleanly. Reads are serialised, which costs nothing at these query
     * shapes and removes a whole class of concurrency bug from a library whose callers cannot see the
     * implementation.
     */
    private val lock = Any()

    /**
     * `mode=ro` rather than a plain path.
     *
     * **Read-only, enforced by the connection rather than by discipline.** A package file is an
     * artifact, and an SDK that can write to it can corrupt the thing it was handed — and a read-only
     * connection also refuses to create the `-wal` sidecar, which a write-capable one would leave
     * beside a file the CLI also writes.
     */
    private val connection = DriverManager.getConnection(
        "jdbc:sqlite:file:${URLEncoder.encode(path, "UTF-8")}?mode=ro"
    )

    /**
     * Probed once.
     *
     * `pragma_module_list` rather than creating a probe table: creating one and creating it again
     * fails, so an uncached probe answers `true` once and `false` forever after, and the symptom is
     * that the first fuzzy search in a session works and every later one silently returns nothing.
     * Reading the module list has no side effect at all, so there is nothing to collide.
     */
    private var probed: Capabilities? = null

    init {
        // sqlite-jdbc bundles its native library; loading the driver by class name works whether or
        // not the consuming application used a JDBC `DriverManager` convention of its own.
        Class.forName("org.sqlite.JDBC")
    }

    override val sqliteVersion: String =
        synchronized(lock) {
            connection.createStatement().use { statement ->
                statement.executeQuery("SELECT sqlite_version()").use { rows ->
                    if (rows.next()) rows.getString(1) else "unknown"
                }
            }
        }

    override fun <T> select(sql: String, bindings: List<SqlValue>, limit: Int, map: (SqlRow) -> T): List<T> =
        synchronized(lock) {
            connection.prepareStatement(sql).use { statement ->
                bindings.forEachIndexed { index, value ->
                    val slot = index + 1
                    when (value) {
                        is SqlValue.Text -> statement.setString(slot, value.value)
                        is SqlValue.Integer -> statement.setLong(slot, value.value)
                        is SqlValue.Real -> statement.setDouble(slot, value.value)
                        SqlValue.Null -> statement.setNull(slot, java.sql.Types.NULL)
                    }
                }
                statement.executeQuery().use { rows ->
                    val out = ArrayList<T>()
                    while (out.size < limit && rows.next()) {
                        out.add(map(JdbcRow(rows)))
                    }
                    out
                }
            }
        }

    override fun capabilities(): Capabilities = synchronized(lock) {
        probed ?: run {
            val modules = connection.createStatement().use { statement ->
                statement.executeQuery("SELECT name FROM pragma_module_list").use { rows ->
                    buildSet { while (rows.next()) add(rows.getString(1)) }
                }
            }
            Capabilities(fts5 = "fts5" in modules, rtree = "rtree" in modules).also { probed = it }
        }
    }

    override fun close() {
        synchronized(lock) { connection.close() }
    }
}

/** One JDBC row, read by zero-based column index. */
private class JdbcRow(private val rows: ResultSet) : SqlRow {
    override fun text(index: Int): String? = rows.getString(index + 1)

    override fun int(index: Int): Long? =
        rows.getLong(index + 1).let { if (rows.wasNull()) null else it }

    override fun double(index: Int): Double? =
        rows.getDouble(index + 1).let { if (rows.wasNull()) null else it }

    override fun isNull(index: Int): Boolean = rows.wasNull()
}