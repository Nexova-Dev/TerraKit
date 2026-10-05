# ADR-007: Language and runtime per component

Status: **Accepted**
Date: 2026-10-05
Affects: `cli/`, `data-pipeline/`, `api/`, `registry/`, `sdk/`

## Context

§43 recommends a specific stack: Node.js/TypeScript/PostgreSQL/Redis for the backend, Python for the
pipeline, a compiled standalone CLI (Rust recommended) distributed for macOS, Linux and Windows,
Swift via SPM, Kotlin via Gradle.

## Decision

| Component | Language | Rationale |
|---|---|---|
| `cli/` | Rust | single static binary, no runtime, `libsqlite3` only |
| `data-pipeline/` | Python | the geospatial data ecosystem is Python; the pipeline is developer-facing, not shipped |
| `api/` | TypeScript | optional hosted infrastructure, never on the SDK's critical path |
| `registry/` | static JSON | no server, no database |
| `sdk/swift/` | Swift | SPM, Foundation, `libsqlite3` |
| `sdk/kotlin/` | Kotlin | Gradle multiplatform (JVM + Android), `SQLite` |

## Rationale per choice

**Rust CLI.** It is distributed as one executable to three platforms with no runtime prerequisite,
which is what §43 asks for and what a CLI distributed by `brew` and `curl` requires.
`rusqlite` with the `bundled` feature removes the system-SQLite version dependency. SHA-256 and
Ed25519 verification come from audited RustCrypto crates rather than hand-rolled code — verification
correctness is not a place to improvise.

**Python pipeline.** GDAL, Fiona, geopandas, pyproj and Shapely are Python. The pipeline is a
developer tool run on maintainers' machines and in CI, never shipped to users, so its startup time
and packaging matter far less than its correctness and its access to the ecosystem.

**Static registry.** Every read path the CLI needs is `index.json` plus per-package manifests on
object storage behind a CDN. A database and an application server would add operational cost and
failure modes without removing any read latency. A dynamic API is needed only when API keys and rate
limiting arrive (§37), and that is a separate deployable reading from the same object storage.

**Zero third-party dependencies in the SDKs.** §23 says "keep dependencies minimal" and §56.14 says
"minimal dependencies". Both SDKs link `libsqlite3` — present on Apple platforms, part of Android, and
available on Linux — and use only standard-library types. No ORM, no query builder, no serialization
library, no HTTP client for the core paths. This is why
[`ADR-003`](ADR-003-search-mechanisms.md) specifies raw prepared statements: adding a query builder
would contradict this table.

## Alternatives considered

**Node.js CLI (via `pkg` / `bun build-compile`).** Rejected. Larger binaries, slower startup, and the
`node_modules` resolution problem makes single-file distribution unreliable across three platforms.

**Go CLI.** Genuinely competitive — comparable single-binary distribution and an excellent SQLite
driver. Rejected because Rust's borrow checker materially helps the package installer, where
verification and extraction are intertwined error handling over a mutable filesystem. Not a strong
enough reason to override §43's explicit recommendation.

**TypeScript SDK.** Deferred to §44's future SDK list. The data format is language-independent, which
is the requirement; no additional design work is needed.

**Kotlin Multiplatform including iOS.** Deferred. JVM + Android covers §24 fully. The native targets
add SKIE and XCFramework complexity before anyone needs them.

## Consequences

**Positive**
- Each component uses an ecosystem that fits its problem.
- The SDKs have zero dependency-management surface, so version conflicts cannot occur for users.
- The registry has no availability-critical component.

**Negative**
- Five languages means five toolchains in CI and five sets of contribution instructions.
- Cross-language normalization consistency is a real risk, addressed by shared golden test vectors in
  `tests/fixtures/` executed for every language
  ([`ARCHITECTURE.md` §8](ARCHITECTURE.md#8-cross-language-determinism)).
- Rust has the steepest contribution curve of the five. Mitigated by keeping the CLI's command
  surface mechanical: fetch, verify, extract, move.

**Neutral**
- SQLite ≥ 3.34 is the floor everywhere, because of the FTS5 trigram tokenizer
  ([`ADR-003`](ADR-003-search-mechanisms.md)).
