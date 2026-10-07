# Contributing to TerraKit

Status: **Draft**
Last updated: 2026-10-05

---

## 1. Read the specification first

TerraKit follows a documentation-first workflow. §55 of the source specification requires
identifying contradictions, missing requirements and technical risks **before** implementation.

These documents are the source of truth:

| Document | Read it for |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Why the system is shaped this way |
| [`DATA_MODEL.md`](DATA_MODEL.md) | Entities, identity, normalization, schema |
| [`PACKAGE_FORMAT.md`](PACKAGE_FORMAT.md) | Package layout, manifests, verification |
| [`core/spec/normalization.md`](core/spec/normalization.md) | The normative normalization algorithm |
| [`docs/adr/`](docs/adr/) | Decisions already made, and what they cost |

**Code that disagrees with these documents is a bug.** If you believe a document is wrong, open an
issue or an ADR proposal first — do not encode the disagreement silently in code.

### Where to start contributing

[`ARCHITECTURE.md` §13](ARCHITECTURE.md#13-open-questions) lists five open questions. They are the
places where a contribution has the most leverage, because judgement is needed and no code exists yet
to push back.

---

## 2. Setup

```bash
git clone https://github.com/NexovaDev/terrakit.git
cd terrakit

# Pipeline (Python 3.12+)
python3 -m venv .venv && source .venv/bin/activate
pip install -r data-pipeline/requirements.txt

# Master database
docker compose -f docker/docker-compose.yml up -d

# CLI (Rust 1.80+)
cd cli && cargo build

# Swift SDK
cd sdk/swift && swift build

# Kotlin SDK
cd sdk/kotlin && ./gradlew build
```

---

## 3. Implementation order

Follow it. Work out of sequence creates work for everyone else.

```text
1. core/schema/          SQLite DDL, versioned. Foundation of everything.    done
2. data-pipeline/        ingest → normalize → validate → publish             done
3. cli/                  read-only commands; add/install over the registry   done
4. registry/             static index + object storage                       done
5. cli/                  add, remove, update                                 done
6. cli/                  cache, bundle                                       done
7. cli/                  install from a GitHub source                        next
8. sdk/swift/            query engine over the produced package
9. sdk/kotlin/           same surface, conformance-tested against Swift
10. Remaining countries  US, GB — plus JP in the test set
11. search depth         autocomplete ranking, boundary polygons
```

The registry came before the remaining CLI commands because they cannot be built or tested without
it: `remove` and `update` are both built on the resolution `add` records.

`add`, `remove`, `update`, `cache` and `bundle` are complete.
[`scripts/test_registry.sh`](scripts/test_registry.sh) builds real packages, publishes them,
installs, updates and removes them, and then attacks the result: a flipped byte, a missing
signature, a manifest edited after signing, and a package signed by an untrusted key must each be
refused with exit `3` and install nothing. A damaged install must be reported rather than called
up to date. Run it after touching [`cli/src/commands.rs`](cli/src/commands.rs),
[`cli/src/install.rs`](cli/src/install.rs), [`cli/src/signature.rs`](cli/src/signature.rs) or
[`tools/build_registry.py`](tools/build_registry.py).

```bash
TERRAKIT_CACHE=~/.cache/terrakit/sources bash scripts/test_registry.sh
```

Steps 8 and 9 are conformance-tested against the **same package bytes**. An implementation that
passes with its own fixture and diverges from the other SDK is not done.

---

## 4. Non-negotiable engineering rules

These are enforced by review and by CI. Numbered as in §56 of the specification.

1. **Offline-first.** No query path may contain a network fallback. If a package is missing, return
   `nil` or throw `.packageNotInstalled` — never reach out.
2. **Never load a whole dataset into memory.** Indexed queries, keyset pagination, hard result caps.
   `database.loadEverything()` does not exist and must not be introduced.
3. **Bound every query.** No public API returns an unbounded collection.
4. **Bound every parameter.** User strings reach SQLite only as bound parameters. String interpolation
   into SQL is a defect, not a style issue.
5. **Stable IDs only.** Never derive an ID from a name. See
   [`ADR-002`](docs/adr/ADR-002-identity-from-external-authorities.md).
6. **Never assume a fixed hierarchy.** Filter by `level` for structure, `type` for meaning.
7. **Version everything.** Software, package format, schema and dataset version separately.
8. **Reproducible builds.** Timestamps from `SOURCE_DATE_EPOCH`, never the wall clock.
9. **Respect licenses.** No source without a resolved license. No override flag exists.
10. **Fail closed.** Verification failures are not warnings.

---

## 5. Tests

### 5.1 Normalization conformance (mandatory)

Anything touching normalization, search or name handling must pass the shared golden vectors:

```bash
python3 tests/conformance/run_normalization.py   # oracle
swift test --filter NormalizationConformance     # Swift
./gradlew :test --tests "*NormalizationConformance*"   # Kotlin
cargo test --manifest-path cli/Cargo.toml --test normalize   # CLI
```

The Rust CLI reads the same `tests/fixtures/normalization.json` through a Cargo integration test, so
its normalizer is held to the identical 48 vectors rather than to a hand-copied subset. That is the
mechanism behind the cross-language determinism claim in
[`ARCHITECTURE.md`](ARCHITECTURE.md#8-cross-language-determinism): a divergence is a CI failure in
whichever implementation is wrong, not a bug report from a user. The dakuten defect
([`DATA_MODEL.md` §1](DATA_MODEL.md#1-core-principle-type-and-level-are-different-things) and
`core/spec/normalization.md` §2) is what this caught before the SDKs existed.

`tests/fixtures/normalization.json` is the specification made executable.

**Never edit the fixture to make a failing test pass.** The fixture defines correct behaviour; a test
failure means the implementation is wrong. Changing the fixture inverts the entire point and will be
rejected in review.

New vectors are welcome — but add them only with a reason tied to real-world data, plus a note on
which implementation was wrong. A vector added to accommodate a buggy implementation makes the
cross-language guarantee weaker, not stronger.

### 5.2 Cross-SDK conformance

`tests/integration/` runs the same query battery against the same package bytes in Swift, Kotlin and
the CLI, comparing **result ID sets**, not just counts. Ordering is deterministic, so exact comparison
is meaningful.

Any divergence between implementations is a defect in the one that deviates from
[`core/spec/normalization.md`](core/spec/normalization.md) or the fuzzy ranking order in
[`DATA_MODEL.md` §9](DATA_MODEL.md#9-search-schema).

### 5.3 Performance

`bench/` measures against the reference fixture. CI fails when p95 exceeds the budgets in §40 of the
specification — 10 ms lookup, 50 ms indexed search, 100 ms autocomplete — and when a memory ceiling
test exceeds its documented bound.

A performance regression is a bug with a number attached. "It got slower" needs a measurement and a
justified threshold change, not a shrug.

### 5.4 Package validation

Any change to the schema, the pipeline or the package builder must keep `terra validate` green with
zero errors on every MVP country fixture. New validation rules need a rule ID, a severity and a test
fixture that triggers them — an untriggered validation rule is untested code.

---

## 6. Pull requests

- One logical change per PR. Refactors separate from behaviour changes.
- Update the affected specification document in the same PR. Documentation is part of the change, not
  a follow-up.
- ADR required for any architectural decision: new technology, new dependency in an SDK, new ID rule,
  new normalization step, new package layout.
- Explain **why**, not just what. The diff already shows what.
- Include the test that proves it, and the measurement if it touches a performance path.
- CI green before review. Failing CI wastes everyone's time.

---

## 7. Commit messages

```text
<type>(<scope>): <subject>

<body explaining why, not what>
```

Types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `chore`, `data`, `adr`.

```text
feat(fts): route 2-char queries to the B-tree prefix index

The FTS5 trigram tokenizer requires 3+ characters to produce a token,
so queries shorter than that fell through to a full scan. Routing by
query length keeps short queries on the index.

Refs ADR-003
```

---

## 8. Reporting bugs

Include:

- TerraKit version, SDK version, package name and dataset version
- OS, Swift or Kotlin version, SQLite version (`terra doctor` prints all of this)
- What you expected, what happened
- A minimal reproduction

For **data quality** — wrong postal codes, broken hierarchy, missing cities — a bug report is the
right channel. Geographic data has errors and they are worth reporting precisely. §30's validator
catches structural problems; it cannot catch a postal code that is structurally valid and
factually wrong. People notice these, and the reports are valuable.

For **security** issues, follow [`SECURITY.md` §9](SECURITY.md#9-reporting-a-vulnerability) instead.

---

## 9. Code of conduct

Be direct and technically honest. Assume good faith. Review the code, not the contributor — and when
reviewing code, say what is wrong and why, not who was wrong.

Disagreement about architecture is expected and healthy: this project has opinions in its ADRs and
they are meant to be challenged. Challenge them with a concrete alternative and its costs. "I would do
it differently" without a trade-off analysis is not a review comment.

---

## 10. Licensing contributions

By contributing you confirm you have the right to license the contribution under the project's terms
(MIT for code) and that it introduces no conflicting obligation. Data contributions must come from a
source whose license permits redistribution, and must include provenance metadata.

Details in [`LICENSING.md` §7](LICENSING.md#7-contributor-licensing).
