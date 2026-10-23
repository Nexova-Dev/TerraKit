# TerraKit Data Pipeline

Status: **Partially implemented**
Last updated: 2026-10-05
Implementation: `data-pipeline/src/terrakit_pipeline/`

```bash
python3 -m terrakit_pipeline build TR --level postal
python3 -m terrakit_pipeline validate datasets/terrakit-data-tr-2026.10.1
```

---

## Stage status

SPEC §27 defines twelve stages. What exists, and what does not:

| # | Stage | State |
|---|---|---|
| 1 | Downloader | done — cached, checksum-recorded, retry-bounded |
| 2 | Parser | done — column counts asserted per file |
| 3 | Normalizer | done — [`core/spec/normalization.md`](core/spec/normalization.md) |
| 4 | Deduplicator | done — keyed on GeoNames id |
| 5 | Hierarchy Builder | done — plus identity resolution and the closure table |
| 6 | Validator | done — SPEC §30 rules, error/warning split |
| 7 | Database Generator | done — applies `core/schema/*.sql` |
| 8 | SQLite Generator | done — fixed layout for reproducibility |
| 9 | Package Builder | done — manifest, checksums |
| 10 | Checksum / Signature | **checksum done, signing not** |
| 11 | Registry | not started — static files, CDN upload |
| — | Localization ingest | **done** — see below |

Stage 10 is split because a signature is only meaningful once a release key exists, and
generating a key nobody has the private half of is worse than not signing. The
verification order, keyring and rotation design are fully specified in
[`SECURITY.md`](SECURITY.md); only the signing call is missing.

---

## Names: done, and it found two real bugs

`place_names` and `place_alternate_names` are populated from `alternateNamesV2`. What that now buys:

| Capability | Before | After |
|---|---|---|
| `Terra.search("Constantinople")` reaching İstanbul | nothing found | İstanbul, `class='historic'` |
| `Terra.search("イスタンブール")` | nothing found | İstanbul |
| `Terra.search("Byzantion")`, `"Byzance"` | nothing found | İstanbul |
| `Terra.localizedName(for: .japanese)` | English name | `イスタンブール` |
| `country.name` for TR | `Turkey` | `Türkiye` |
| `country.name` for GE | `Georgia` | `საქართველო` |
| `country.name` for DE | `Germany` | `Deutschland` |

Measured on the built packages: TR 20 769 localized names across 245 languages, DE 49 305 across 289,
GE 3 271 across 264.

### Two defects this stage exposed

**The dakuten bug in normalization itself.** NFKD decomposes Japanese kana with a voiced sound mark
into a base kana plus a category `Mn` combining mark:

```text
ガ → 'カ' + U+3099        ガザ → 'カサ'      (Gaza became "Kasa")
```

Stripping every combining mark — which is exactly what step 2 of the specification said to do — rewrote
Japanese place names into *different* place names and collapsed `カ` and `ガ` onto one search key.
Japanese search returned nothing while `localizedName(for: .japanese)` worked, which is what surfaced
it. Fixed by stripping a mark only when it modifies a Latin, Greek or Cyrillic base, keeping marks
word-internal in step 6, and recomposing to NFC in a new step 9.

This is a change to `core/spec/normalization.md`, not just to the code, and it adds 9 conformance
vectors (40–48). It is the strongest argument for the conformance suite existing: the defect sat in the
one algorithm three future SDKs would have implemented from the same specification.

**Localized names were not searchable.** `place_names` rows are the authoritative per-language name, so
they were going into the display table only. `Terra.search("イスタンブール")` found nothing even though
`localizedName(for: .japanese)` returned the right answer — one API resolving a place and another not,
from the same package. The search index now covers all three name tables.

### What the filter throws away, and why

About 70% of a country's rows: no language and no discipline flag. Those are unverified user
submissions — "Antaliyah", "Istambul", bare Wikipedia links. Two identifier classes are additionally
marked `isPreferredName` upstream, so a naive ingest can put a Wikipedia URL in a name column.

The full measured breakdown of `isolanguage`, including the bibliographic ISO 639-2 codes that look
like identifiers but are real languages, is in
[`sources/README.md`](data-pipeline/sources/README.md#alternatenamesv2--the-largest-source-and-the-most-dangerous).

### Cost

Packages roughly doubled in raw size and compress about 5.9× with zstd, so the download cost is
modest:

| Country | Raw before | Raw after | zstd after |
|---|---|---|---|
| TR | 6.0 MB | 9.8 MB | 1.6 MB |
| DE | 13 MB | 23.3 MB | 3.9 MB |
| GE | 0.5 MB | 1.3 MB | 0.2 MB |

Keeping all 289 languages rather than a curated handful is a deliberate choice: SPEC §7 asks for
localized names without restricting languages, and trimming to "popular" ones would be an arbitrary
lossy decision made to save bytes.

---

## Design decisions worth knowing

### Identity is resolved, never derived

[`DATA_MODEL.md` §3](DATA_MODEL.md#3-identity) and
[`ADR-002`](docs/adr/ADR-002-identity-from-external-authorities.md). Priority order:
ISO 3166-2, then GeoNames id, then a rate-limited deterministic hash. Measured across the
four MVP countries, **zero** places needed the hash fallback, which means the source data
is strong enough to support stable identity and delta updates
([`ADR-006`](docs/adr/ADR-006-row-level-deltas.md)).

### The country is part of the hierarchy

SPEC §4 puts `Country` inside the hierarchy. The country row is at level 1 and every
closure chain terminates there. Georgia has three settlements whose `admin1_code` is
`"00"`, which does not exist in `admin1CodesASCII.txt`; without the anchor they had
nowhere to attach and were reported as orphans. They now attach to the country and their
level drops to 2, because that is the only honest description of where they sit.

### Settlement type comes from population, not from the feature code

GeoNames marks a 400-person hamlet that happens to be a district seat as `PPLA2`. Using
that to assign `city` would make a hamlet outrank a real city in results, contradicting
the population-based `search_rank` built two lines later. Thresholds: ≥100 000 city,
≥5 000 town, below village.

### Level follows the parent, always

A settlement whose parent is a province is at the province's child level, and an `admin2`
with no known province sits at level 2. Hardcoding "admin2 is level 3" produced a level-3
row under a level-1 parent, which the validator correctly rejected.

### Postal primary is decided once per code

Deciding `is_primary` per source record marked every district a code touches primary, so
`postalCodes(city:)` returned several unrankable results. Primary is now resolved after
all records for a code are seen, and it is always the most specific candidate.

### The shipped DDL is the authoritative DDL

The pipeline applies `core/schema/*.sql` rather than carrying its own copy. A second copy
would drift, and drift is discovered by users hitting `minReaderVersion` in the field
rather than in CI.

### Reproducibility is structural

Fixed page size, `journal_mode=DELETE`, explicit `VACUUM`, insertion in `(level, id)`
order, `SOURCE_DATE_EPOCH` for every timestamp, CalVer derived from input content. Verified:
`scripts/check_reproducible.sh` builds twice and compares SHA-256.

`rowid_int` follows the same `(level, id)` total order, so the R\*Tree mapping is
reproducible too.

---

## Reproducibility

Fixed page size, `journal_mode=DELETE`, explicit `VACUUM`, insertion in `(level, id)`
order, `SOURCE_DATE_EPOCH` for every timestamp, CalVer derived from input content. Verified:
`scripts/check_reproducible.sh` builds twice and compares SHA-256.

## Validation

SPEC §30 rules plus two additions, both to make §29 enforceable:

- **every `data_sources` row needs a resolved license** — the build fails without one,
  and there is no override flag ([`LICENSING.md` §6](LICENSING.md#6-pipeline-enforcement))
- **hash-derived ID ratio must stay under 0.5%** — above that, the source data is too
  weak to support stable identity

Settlement counts in the report are by **type**, not level. Türkiye's largest cities sit
at level 3, so counting level 5 printed `Cities: 0` for a package containing 112 of them.

## Tests

```bash
./scripts/test.sh                                  # everything
python3 tests/conformance/run_normalization.py     # the spec oracle
python3 tests/conformance/test_phonetic.py
python3 data-pipeline/tests/test_pipeline.py
bash scripts/test_schema.sh
./scripts/check_reproducible.sh TR
```

Fixtures are synthetic and deliberately include what breaks naive code: a missing
upstream reference, a postal code across two districts, duplicate sibling names, a
non-Latin name.

## Upstream data defects

Each was hit while building an MVP country, and each is recorded in
[`sources/README.md`](data-pipeline/sources/README.md) with its handling. Summary: Georgia has no
postal file; three Georgian settlements reference a non-existent `admin1_code`; one
Turkish settlement carries `Asia/Baghdad`; `countryInfo.txt` has 19 columns rather than
the 47 the upstream docs suggest; and `admin1_code`/`admin2_code` use different key spaces
in the same file.

None of these are worked around silently. They are warned about, and where the correct
behaviour is ambiguous the pipeline says which choice it made and why.

## Source archive cache contract

The downloader extracts only `cities500.txt` from the upstream ZIP and records the
checksum and size of that extracted payload. Postal sources remain ZIP archives named
`<ISO2>.zip`, as expected by the postal parser and offline builds. Both online and
`--skip-download` builds repair legacy caches containing ZIP bytes in `cities500.txt`
or a postal archive incorrectly named `<ISO2>.txt`. Archive member paths are never
extracted to the filesystem.
