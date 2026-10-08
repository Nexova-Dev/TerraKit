/// A thin, read-only `sqlite3` wrapper.
///
/// Deliberately thin. It exists to make statements, bindings and row mapping safe and to hold one
/// connection, and nothing more — a query builder here would be a second data-access layer that
/// has to be kept in step with the SQL in the query engine.
///
/// **Read-only, enforced by the open mode rather than by discipline.** A package file is an
/// artifact, and an SDK that can write to it can corrupt the thing it was handed. `SQLITE_OPEN_READONLY`
/// also refuses to create a `-wal` sidecar, which `read_only(true)` in other bindings still allows.
///
/// **One connection, behind a lock.** `SDK_SPEC` §11: SQLite in serialized mode is fast enough at
/// these query shapes that pooling would be premature, and the connection count stays at one
/// because that is what `sqlite3_threadsafe` supports cleanly. Reads are serialised, which costs
/// nothing at these query shapes and removes a whole class of concurrency bug from a library whose
/// callers cannot see the implementation.

import Foundation
import SQLite3

/// SQLite's default string destructor, in the shape Swift's importer expects.
///
/// `SQLITE_TRANSIENT` tells SQLite to copy the bytes, because Swift's `String` storage is not
/// guaranteed to outlive the statement. Without it a deallocated buffer is read at execution time,
/// which is a use-after-free that shows up as a wrong answer rather than a crash.
private let sqliteTransient = unsafeBitCast(
    -1, // SQLITE_TRANSIENT
    to: sqlite3_destructor_type.self
)

/// One read-only connection to one package database.
public final class Database: @unchecked Sendable {
    private var handle: OpaquePointer?
    private let lock = NSLock()

    public let url: URL
    /// The package's ISO 3166-1 alpha-2 code.
    public let countryCode: String

    /// Open a package read-only.
    ///
    /// - Throws: `.databaseUnreadable` when the file is absent or is not a SQLite database,
    ///   `.unsupportedSQLiteVersion` below 3.34 — the version the trigram tokenizer needs.
    public init(url: URL, countryCode: String) throws {
        self.url = url
        self.countryCode = countryCode

        let flags = SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX
        guard sqlite3_open_v2(url.path, &handle, flags, nil) == SQLITE_OK, handle != nil else {
            let message = handle.map { String(cString: sqlite3_errmsg($0)) } ?? "unknown"
            sqlite3_close_v2(handle)
            handle = nil
            throw TerraError.databaseUnreadable(underlying: "\(url.lastPathComponent): \(message)")
        }

        let version = try Database.sqliteVersion()
        guard Database.isVersionSupported(version) else {
            sqlite3_close_v2(handle)
            handle = nil
            throw TerraError.unsupportedSQLiteVersion(found: version, required: "3.34")
        }

        // The connection count stays at one; SQLite's own page cache is the right cache here
        // because the working set is a single small file.
        sqlite3_exec(handle, "PRAGMA cache_size = -8000", nil, nil, nil)
        sqlite3_exec(handle, "PRAGMA temp_store = MEMORY", nil, nil, nil)
    }

    deinit {
        sqlite3_close_v2(handle)
    }

    public static func sqliteVersion() throws -> String {
        var handle: OpaquePointer?
        guard sqlite3_open_v2(":memory:", &handle, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else {
            throw TerraError.databaseUnreadable(underlying: "cannot open an in-memory database")
        }
        defer { sqlite3_close_v2(handle) }
        return String(cString: sqlite3_libversion())
    }

    /// The trigram tokenizer the search index depends on arrived in 3.34.
    ///
    /// Checked rather than assumed: an SDK that opens a package on an older SQLite fails on the
    /// first search, with an FTS5 error that says nothing about the version.
    public static func isVersionSupported(_ version: String) -> Bool {
        let parts = version.split(separator: ".").compactMap { Int($0) }
        guard parts.count >= 2 else { return false }
        if parts[0] > 3 { return true }
        if parts[0] < 3 { return false }
        return parts[1] >= 34
    }

    /// Cached capability probe. `nil` until probed.
    private var capabilities: (fts5: Bool, rtree: Bool)?

    /// Whether this build has FTS5 with the trigram tokenizer and R\*Tree.
    ///
    /// Probed rather than compiled-for: both are optional SQLite features, and a diagnostics screen
    /// needs to report what is actually available on the machine it is running on.
    ///
    /// **Probed once and cached.** The probe creates a temp table, and creating the same temp
    /// table twice fails with "table already exists" — so an uncached probe returns `true` on the
    /// first call and `false` on every call after it. The symptom is that the first fuzzy search in
    /// a session works and every later one silently returns nothing, which looks exactly like a
    /// missing index rather than a bug in the check.
    public func hasSearchIndexes() -> (fts5: Bool, rtree: Bool) {
        lock.lock()
        defer { lock.unlock() }
        if let cached = capabilities { return cached }

        guard let handle else { return (false, false) }
        let fts5 = sqlite3_exec(
            handle,
            "CREATE VIRTUAL TABLE temp.terrakit_probe_fts USING fts5(x, tokenize='trigram')",
            nil, nil, nil
        ) == SQLITE_OK
        let rtree = sqlite3_exec(
            handle,
            "CREATE VIRTUAL TABLE temp.terrakit_probe_rtree USING rtree(id, a, b, c, d)",
            nil, nil, nil
        ) == SQLITE_OK

        let result = (fts5: fts5, rtree: rtree)
        capabilities = result
        return result
    }

    // MARK: - Statements

    /// A prepared statement, ready to be stepped.
    ///
    /// Bindings are indexed from 1, which is SQLite's convention. `SQLITE_TRANSIENT` on every
    /// string binding: see the note at the top of this file.
    public final class Statement {
        fileprivate let handle: OpaquePointer
        private let statement: OpaquePointer

        init(handle: OpaquePointer, sql: String) throws {
            self.handle = handle
            var statement: OpaquePointer?
            let rc = sqlite3_prepare_v2(handle, sql, -1, &statement, nil)
            guard rc == SQLITE_OK, let statement else {
                throw TerraError.databaseUnreadable(
                    underlying: "cannot prepare: \(String(cString: sqlite3_errmsg(handle)))"
                )
            }
            self.statement = statement
        }

        deinit {
            sqlite3_finalize(statement)
        }

        public func bind(_ value: String, at index: Int32) {
            sqlite3_bind_text(statement, index, value, -1, sqliteTransient)
        }

        public func bind(_ value: Int64, at index: Int32) {
            sqlite3_bind_int64(statement, index, value)
        }

        public func bind(_ value: Double, at index: Int32) {
            sqlite3_bind_double(statement, index, value)
        }

        /// Bind `nil`, distinct from binding an empty string.
        public func bindNull(_ index: Int32) {
            sqlite3_bind_null(statement, index)
        }

        /// Advance one row. `false` at the end of the result set.
        public func step() throws -> Bool {
            let rc = sqlite3_step(statement)
            switch rc {
            case SQLITE_ROW: return true
            case SQLITE_DONE: return false
            default:
                throw TerraError.databaseUnreadable(
                    underlying: String(cString: sqlite3_errmsg(handle))
                )
            }
        }

        /// Reset for re-execution, keeping the bindings.
        public func reset() {
            sqlite3_reset(statement)
        }

        // MARK: Column access

        public func string(_ index: Int32) -> String? {
            guard let text = sqlite3_column_text(statement, index) else { return nil }
            return String(cString: text)
        }

        public func int(_ index: Int32) -> Int64? {
            guard sqlite3_column_type(statement, index) != SQLITE_NULL else { return nil }
            return sqlite3_column_int64(statement, index)
        }

        public func double(_ index: Int32) -> Double? {
            guard sqlite3_column_type(statement, index) != SQLITE_NULL else { return nil }
            return sqlite3_column_double(statement, index)
        }

        public func isNull(_ index: Int32) -> Bool {
            sqlite3_column_type(statement, index) == SQLITE_NULL
        }
    }

    /// Prepare a statement under the connection lock.
    ///
    /// The lock is released before the statement runs. `SQLITE_OPEN_FULLMUTEX` puts the connection
    /// itself in serialized mode, so concurrent stepping is safe; holding the lock across a query
    /// would serialise reads for no gain.
    public func prepare(_ sql: String) throws -> Statement {
        lock.lock()
        defer { lock.unlock() }
        guard let handle else {
            throw TerraError.databaseUnreadable(underlying: "connection is closed")
        }
        return try Statement(handle: handle, sql: sql)
    }

    /// Run a query and map each row.
    ///
    /// - Parameter limit: a hard ceiling, per SDK_SPEC §12. There is no `allStreets`.
    public func query<T>(
        _ sql: String,
        bindings: [Value] = [],
        limit: Int = QueryLimits.maximumEntities,
        map: (Statement) throws -> T
    ) throws -> [T] {
        let statement = try prepare(sql)
        for (offset, value) in bindings.enumerated() {
            let index = Int32(offset + 1)
            switch value {
            case .text(let text): statement.bind(text, at: index)
            case .integer(let number): statement.bind(number, at: index)
            case .real(let number): statement.bind(number, at: index)
            case .null: statement.bindNull(index)
            }
        }

        var rows: [T] = []
        rows.reserveCapacity(min(limit, 128))
        while rows.count < limit, try statement.step() {
            rows.append(try map(statement))
        }
        return rows
    }

    /// The first row, or `nil`.
    public func first<T>(
        _ sql: String,
        bindings: [Value] = [],
        map: (Statement) throws -> T
    ) throws -> T? {
        try query(sql, bindings: bindings, limit: 1, map: map).first
    }
}

/// A bound query parameter.
public enum Value: Sendable {
    case text(String)
    case integer(Int64)
    case real(Double)
    case null
}

extension Value {
    static func text(_ value: String?) -> Value {
        value.map(Value.text) ?? .null
    }

    static func integer(_ value: Int?) -> Value {
        value.map { Value.integer(Int64($0)) } ?? .null
    }
}

/// Hard limits, `SDK_SPEC.md` §12.
///
/// Enforced, not documented-and-hoped-for.
public enum QueryLimits {
    /// Ceiling on any list-returning call.
    public static let maximumEntities = 1000

    /// Autocomplete runs on every keystroke, so its cap is much lower.
    public static let autocomplete = 50

    /// Ceiling on candidates pulled from the R\*Tree before distance is computed.
    ///
    /// The R\*Tree is a candidate generator, never the answer
    /// ([`ADR-005`](../../docs/adr/ADR-005-postgis-sqlite-rtree.md)): a bounding-box prefilter
    /// degrades at high latitudes, exactly where data is densest. So more candidates are taken
    /// and then ordered by haversine in SDK code.
    public static let nearestCandidates = 400
}