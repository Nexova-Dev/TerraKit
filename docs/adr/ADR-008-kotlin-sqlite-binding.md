# ADR-008: How the Kotlin SDK reaches SQLite

Status: **Accepted**
Date: 2026-10-08
Decides: the SQLite binding for `sdk/kotlin/`, and whether the Android target is required to build

---

## Context

[`ADR-007`](ADR-007-language-stack.md) fixed the language stack and, in the same decision table,
two things that collide once the Kotlin SDK is actually written:

> `sdk/kotlin/` | Kotlin | Gradle multiplatform (JVM + Android), `SQLite`

> **Zero third-party dependencies in the SDKs.** §23 says "keep dependencies minimal" and §56.14 says
> "minimal dependencies". Both SDKs link `libsqlite3` — present on Apple platforms, part of Android,
> and available on Linux — and use only standard-library types.

The Swift SDK resolves this because Apple platforms ship the SQLite C library and Foundation's
`String` provides NFKD through ICU. Kotlin has no equivalent on either target:

* **Android** — `android.database.sqlite` is in the platform, so "part of Android" holds exactly as
  ADR-007 says. Zero dependencies.
* **JVM** — the JDK ships **no** SQLite driver. There is no `java.sql` implementation of it and never
  has been. Something has to provide the binding.

So "zero third-party dependencies" and "JVM target" cannot both be literally true. One of the two has
to give, and which one is a decision rather than an implementation detail.

The JVM target is not optional to the project. [`SDK_SPEC.md` §14](../../SDK_SPEC.md) requires the
same battery to run against the same package bytes in the CLI, Swift and Kotlin, and CI runs it on
Linux where there is no Android runtime. A Kotlin SDK that could only be tested on an emulator would
not be tested at all.

## Decision

**The Kotlin SDK depends on exactly one artifact: `org.xerial:sqlite-jdbc`, on the JVM target only.**
Nothing is added on Android, and nothing is added above the binding.

The reasoning is that `sqlite-jdbc` occupies the same position in the JVM's stack that `libsqlite3`
occupies in the Swift SDK's. It is the platform's binding to the SQLite **C library** — it ships
native binaries and exposes a JDBC `Driver` for them. It is not a data-access layer, and the
distinction is what ADR-007's constraint is actually about:

* it does not build queries — every statement is a prepared statement with bound parameters, as
  [`ADR-003`](ADR-003-search-mechanisms.md) already requires of the Rust and Swift sides
* it does not map rows to objects — every entity is assembled by hand from fixed columns
* it does not serialize, cache, log or open a socket
* adding it cannot cause a version conflict between two libraries in a user's build, because there is
  only one and it is pinned

What ADR-007 rules out stays ruled out: no ORM, no query builder, no serialization library, no HTTP
client on any path. A consumer's dependency tree gains one transitive native library and nothing
else.

The floor is SQLite ≥ 3.34, which `sqlite-jdbc` 3.41 satisfies. A JVM that resolves to an older
`sqlite-jdbc` — a consumer pinning one for an unrelated reason — gets
`TerraError.UnsupportedSqliteVersion` rather than an FTS5 error that says nothing about versions,
the same failure mode `Database.isVersionSupported` guards on the Swift side.

### The Android target is conditional

`androidTarget()` is declared only when an Android SDK is discoverable, through `ANDROID_HOME`,
`ANDROID_SDK_ROOT` or `sdk.dir` in a local `local.properties`.

This is not a preference for a tidier build file. A hard Android target makes the entire build
unrunnable on any machine without the SDK, and the consequence is concrete: the conformance suite
would silently stop running on Linux CI, and "the Kotlin SDK is conformance-tested" would become a
claim with no job enforcing it. A build that cannot run its own tests on the platform its tests
target is worse than a build with one fewer target available.

The cost is real and is accepted: on a machine without the SDK, `assembleAndroid` does not exist and
the Android adapter is not compiled. The mitigation is that the Android module is a **thin adapter**
— it implements one `expect`ed interface and contains no query logic, no normalization and no
ranking, so there is very little in it that the JVM suite does not already exercise. All of it is
data access over an API that is documented and stable.

The alternative — a separate CI job with the Android SDK, and a hard Android target everywhere — is
the better long-term shape and is recorded here as the reason to revisit. It is not what ships now,
because a green build that skips the tests is worse than a build that runs them.

## Consequences

**The dependency count is one, and it is honest.** `SDK_SPEC.md` §1.6 currently reads "zero
third-party dependencies beyond `libsqlite3` and the standard library". On the JVM that clause is
false, so the spec is corrected in the same change to name `sqlite-jdbc` explicitly rather than being
left to be discovered by whoever adds the dependency and finds a rule that forbids it.

**SQLite versions can differ per platform.** The Swift SDK links the system SQLite, the JVM SDK
bundles 3.41 inside `sqlite-jdbc`, and Android ships whatever the device has. Package bytes are
identical across all three — that is the property the conformance suite exists to check — but the
**engine** underneath differs, so a query that depends on a SQLite behaviour added after 3.41 would
be a portability defect. `Database.hasSearchIndexes` is checked on every platform for that reason, and
it is a probe rather than an assumption precisely because the engine is not the same everywhere.

**Consequences of accepting this alternative.** A consumer who needs to guarantee the SQLite version
exactly must pin `sqlite-jdbc`, and an offline or air-gapped build must vendor the native classifier
for their platform. `sqlite-jdbc` publishes natives for Linux, macOS and Windows across x86-64 and
aarch64, which covers what §24 asks for, but it is a dependency that has to be resolvable.

**Consequences of not choosing.** Android-only would have kept the dependency count at zero and made
the Kotlin SDK untestable on any platform CI can run, which would have left §14's cross-SDK
comparison unenforced — the single property that justifies having two SDKs rather than one.