# ADR-003: Three search mechanisms, computed at build time

Status: **Accepted**
Date: 2026-10-05
Affects: `DATA_MODEL.md` §5, §9, all SDKs

## Context

§16 requires exact, prefix, fuzzy, localized and transliterated search, with country and city
filtering and result ranking, at <50 ms. §17 requires structured address autocomplete at <100 ms.
§8 requires `Istanbul`, `İstanbul`, `istanbul` and `ISTANBUL` to resolve identically, and `Atatürk` /
`Ataturk` to produce relevant results.

No single SQLite search mechanism satisfies all of this.

## Decision

Decompose into three, all indexed at package build time:

| Need | Mechanism |
|---|---|
| Exact, prefix | B-tree on `places.name_norm`, composite `(name_norm, type)` |
| Fuzzy, substring, infix | FTS5 with the `trigram` tokenizer |
| Typo tolerance | precomputed Double Metaphone codes in `phonetic_codes` |
| Ranking | precomputed integer `search_rank` |

**The SDKs never compute phonetic codes and never transliterate.** Both are computed by the pipeline
and shipped in the package. The only normalization an SDK performs is on the user's query string.

## Why computation moves to build time

Implementing Double Metaphone correctly in Swift, Kotlin and Rust — then keeping all three
byte-identical — is an ongoing liability. Three implementations, three sets of edge cases around
Turkish and German phonology, and no way for a user to report a mismatch other than as a search bug.
Moving it to the pipeline makes agreement a data property that CI verifies once.

The same argument applies to transliteration. §5.2 relies on GeoNames-provided transliterations
stored as indexed rows rather than an in-SDK transliteration engine, for the identical reason.

Ranking is precomputed for an index reason rather than a consistency one: ranking computed in SQL at
query time cannot use an index. §40's <50 ms budget leaves no room for sorting a candidate set in
Swift or Kotlin at query time.

## Alternatives considered

**FTS5 `unicode61` with `remove_diacritics=2`.** Rejected as the sole mechanism. Handles diacritics
and prefix, but gives no fuzzy or typo tolerance. `remove_diacritics=2` also folds characters beyond
diacritics in ways that need per-language verification.

**FTS5 alone with trigram.** Rejected as the sole mechanism. Trigram needs 3+ characters, so short
queries degrade to scans, and trigram offers no ranking signal.

**SQLite `LIKE 'x%'` with a custom collation.** Rejected. A custom collation must exist in every SDK
and in the pipeline, and cannot be registered on a read-only database opened by an untrusted caller.

**Levenshtein via a UDF.** Rejected. Requires per-row distance computation over candidate sets —
the opposite of indexed.

**sqflite / GRDB query builders.** Rejected for core query paths. GRDB is a reasonable ergonomic
layer, but it adds a dependency to a package whose selling point is minimal dependencies (§23, §56.14).
Raw prepared statements, with a thin shared query helper, keep the SDKs auditable.

## Consequences

**Positive**
- Package size grows. Normalized names, phonetic codes and FTS5 roughly double the raw SQLite size.
  zstd at level 19 recovers most of it (TR `postal`: 13.9 MB → 4.1 MB).
- SDK dependencies stay at zero beyond the SQLite C library.
- Search behaviour is identical across Swift, Kotlin and the CLI, because it is the same bytes.

**Negative**
- The `search_fts` virtual table and `phonetic_codes` table are useless for `--basic` packages. The
  SDK must check `manifest.features` and report `featureUnavailable` rather than assuming a table
  exists.
- The trigram tokenizer requires SQLite ≥ 3.34. Verified available on the target platform (3.54,
  `ENABLE_FTS5`, `ENABLE_RTREE`), but this sets the floor for every platform including older Android.
- Trigram indexes are large. A full-world `streets` package with trigram over 8 million street names
  would need a size and build-time budget before shipping — one more reason streets stay out of MVP.

**Neutral**
- Query length changes the strategy: ≥3 characters go to FTS5, shorter queries to the B-tree prefix
  index. Documented API behaviour, not a hidden fallback.
- Fuzzy result order is fully deterministic — exact, prefix, trigram by phonetic distance, then
  population rank, tie-broken by `id` — so the cross-SDK conformance suite can compare exact result
  sets rather than just counts.
