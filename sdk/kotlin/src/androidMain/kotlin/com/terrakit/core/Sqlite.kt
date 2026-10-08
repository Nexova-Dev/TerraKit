package com.terrakit.core

import android.content.Context
import android.database.Cursor
import android.database.sqlite.SQLiteDatabase
import java.io.File

/**
 * Android's SQLite binding, through `android.database.sqlite`.
 *
 * In the platform, so nothing is added to a consuming app's dependencies — the counterpart to the
 * JVM's single `sqlite-jdbc` artifact in
 * [ADR-008](../../../docs/adr/ADR-008-kotlin-sqlite-binding.md).
 *
 * **The queries, models, normalization and ranking are all in the shared source set** and are
 * exercised by the conformance suite on the JVM. What is platform-specific is this binding, and
 * Android's engine version differs from the JVM's bundled one — which is why `Diagnostics.sqliteVersion`
 * is read from the live connection rather than from a constant.
 */
internal actual fun openSqlite(path: String, countryCode: String): SqlHandle =
    AndroidHandle(path, countryCode)

private class AndroidHandle(path: String, private val countryCode: String) : SqlHandle {

    /** One connection behind a lock, for the same reason as the JVM handle. */
    private val lock = Any()

    /**
     * `OPEN_READONLY`.
     *
     * Enforced by the open mode rather than by discipline: a package file is an artifact, and an SDK
     * that can write to it can corrupt the thing it was handed.
     */
    private val database: SQLiteDatabase =
        SQLiteDatabase.openDatabase(path, null, SQLiteDatabase.OPEN_READONLY)

    private var probed: Capabilities? = null

    /**
     * Android reports its SQLite version as an integer such as `3045000`, not as `3.45.0`.
     *
     * Converted rather than stringified: `Database`'s floor check parses a dotted version, and
     * handing it `3045000` would read as "major 3045000" and pass on any device, including ones below
     * the floor.
     */
    override val sqliteVersion: String = synchronized(lock) { encodeVersion(database.version) }

    private fun encodeVersion(value: Int): String {
        val major = value / 1_000_000
        val minor = (value / 1_000) % 1_000
        val patch = value % 1_000
        return "$major.$minor.$patch"
    }

    /**
     * Values are rendered into the SQL rather than bound. See [SqlLiteral] for why: Android's only
     * public cursor-returning API binds every argument as text, and a text bound silently breaks
     * comparison against a REAL column, which is what `nearest` does.
     *
     * The JVM binding does bind, and `AndroidLiteralRenderingMatchesPreparedStatements` runs the same
     * queries both ways against the same package to prove the two agree on results — so this is a
     * difference in mechanism, not in the contract `SDK_SPEC.md` §1 requires the SDKs to share.
     */
    override fun <T> select(sql: String, bindings: List<SqlValue>, limit: Int, map: (SqlRow) -> T): List<T> =
        synchronized(lock) {
            database.rawQuery(SqlLiteral.render(sql, bindings), null).use { cursor ->
                val out = ArrayList<T>()
                while (out.size < limit && cursor.moveToNext()) {
                    out.add(map(AndroidRow(cursor)))
                }
                out
            }
        }

    override fun capabilities(): Capabilities = synchronized(lock) {
        probed ?: run {
            // Read through the module list rather than by creating a probe table: creating one and
            // creating it again fails, so an uncached probe answers `true` once and `false` forever
            // after, and the symptom is that the first fuzzy search works and every later one silently
            // returns nothing.
            val modules = buildSet {
                database.rawQuery("PRAGMA module_list", null).use { cursor ->
                    val nameColumn = cursor.getColumnIndex("name")
                    if (nameColumn >= 0) {
                        while (cursor.moveToNext()) add(cursor.getString(nameColumn))
                    }
                }
            }
            Capabilities(fts5 = "fts5" in modules, rtree = "rtree" in modules).also { probed = it }
        }
    }

    override fun close() {
        synchronized(lock) { database.close() }
    }
}

private class AndroidRow(private val cursor: Cursor) : SqlRow {
    override fun text(index: Int): String? = if (cursor.isNull(index)) null else cursor.getString(index)

    override fun int(index: Int): Long? = if (cursor.isNull(index)) null else cursor.getLong(index)

    override fun double(index: Int): Double? = if (cursor.isNull(index)) null else cursor.getDouble(index)

    override fun isNull(index: Int): Boolean = cursor.isNull(index)
}

/**
 * Copy packages out of the APK so SQLite can open them.
 *
 * **Required on Android, and not an optimisation.** A database inside a compressed asset is not
 * addressable: SQLite reads the file by path and by page, and an entry inside a zip has neither a
 * stable path nor a fixed offset. `terrakit bundle --platform android` produces the assets; this is
 * the other half.
 *
 * The copy is idempotent and cheap to check — a version directory's existence is the test — so it is
 * safe to call on every launch. Copied files are private to the app, which is also what keeps a
 * package from being modified by another app.
 *
 * ```kotlin
 * val packages = AssetInstaller.install(context, "terrakit")
 * Terra.configure(packages)
 * ```
 *
 * @param assetsPath the directory inside `assets` the packages were bundled into.
 * @return a directory in the app's private files area, holding one subdirectory per country.
 */
object AssetInstaller {
    /**
     * Copy every package under [assetsPath] into the app's files directory, if not already there.
     *
     * @return the destination directory, which may be empty when the APK carries no packages — a
     * consumer that ships none gets a typed error from its first query rather than an empty result.
     */
    @JvmStatic
    @JvmOverloads
    fun install(context: Context, assetsPath: String = "terrakit"): File {
        val destination = File(context.filesDir, "terrakit-packages")
        if (!destination.isDirectory && !destination.mkdirs()) {
            throw TerraError.DatabaseUnreadable("cannot create ${destination.path}")
        }
        copyTree(context, assetsPath, destination)
        return destination
    }

    private fun copyTree(context: Context, relative: String, destination: File) {
        val entries = try {
            context.assets.list(relative) ?: emptyArray()
        } catch (error: Throwable) {
            // A missing assets directory means the app bundles no packages. Not an error: the
            // consumer's first query will say so with the right message.
            return
        }
        if (entries.isEmpty()) {
            copyFile(context, relative, destination)
            return
        }
        for (entry in entries.sorted()) {
            val child = if (relative.isEmpty()) entry else "$relative/$entry"
            val target = File(destination, entry)
            if (target.exists()) continue
            copyTree(context, child, target)
        }
    }

    private fun copyFile(context: Context, relative: String, destination: File) {
        destination.parentFile?.mkdirs()
        context.assets.open(relative).use { input ->
            destination.outputStream().use { output -> input.copyTo(output) }
        }
    }
}