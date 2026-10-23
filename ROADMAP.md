# TerraKit completion roadmap

Audit date: 2026-10-23. This tracks the implemented MVP against the original product
specification and the narrower scope accepted in ARCHITECTURE.md and the ADRs.
Presence of code is not evidence of a production release.

## Implemented

- Python ingestion, normalization, hierarchy construction, validation and package publishing.
- SQLite schema with FTS5 search and R*Tree spatial indexing.
- Rust CLI query, package lifecycle, cache, bundling and GitHub installation commands.
- Signed static registry generation and hostile-package integration checks.
- Swift and Kotlin SDKs with shared normalization and query conformance fixtures.
- US and GB ingestion handling and conformance cases exist alongside TR, GE, DE and JP.

## Release gates, in priority order

1. Run package builds and validation for TR, GE, DE, US, GB and JP. Require zero errors;
   review and document warnings. CI now requests all six countries, including in the SDK job.
   The local datasets directory is empty; this audit does not certify those builds.
2. Run the registry integration suite and cross-SDK battery against those exact package bytes.
   Preserve checks for untrusted signatures, archive traversal, corruption and atomic installation.
   Recheck reproducibility against unchanged upstream inputs.
3. Implement the benchmark harness and document reference hardware and fixtures. Measure p95
   lookup, search and autocomplete against the 10/50/100 ms targets, and establish a measured
   peak-memory budget. Neither bench/ nor a memory regression gate currently exists.
4. Add runnable Swift and Android examples covering installation, bundling, offline search,
   localization, postal lookup and attribution. The examples directory is currently empty.
   Verify Android on a device/emulator, including asset installation.
5. Validate distribution: macOS/Linux/Windows CLI artifacts, Swift package consumption and
   Kotlin/Android publication. Record supported versions and clean-install instructions.
6. Configure production registry hosting and release signing, then smoke-test an install from
   the deployed registry. Development-key acceptance is not production readiness.
7. Finish getting-started documentation, FAQ and release notes. Reconcile stale counts and
   completion claims with actual CI results before declaring version 1.0.

## Audit verification

The local runner passed Python normalization, phonetic, pipeline, name ingestion, schema,
documentation-link and Rust test suites. Registry and cross-SDK tests need built packages.
The initial SDK run hit sandbox restrictions on toolchain caches; those failures do not establish
SDK correctness or indicate a missing SQLite module. Full SDK verification remains a release gate.

## Scope decisions

The original specification lists streets among MVP entities, while ADR-004 explicitly defers
OSM-derived street data. Follow that accepted scope until a new decision resolves ingestion and
licensing. Boundaries, geocoding, routing, delta updates, hosted API, admin dashboard and additional
SDK languages remain later work. The empty api/ directory is therefore an intentional scope gap,
not a reason to make the offline MVP depend on a server.
