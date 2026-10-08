# Query Normalization Specification

Version: 1
Status: **Normative**
Affects: `sdk/swift`, `sdk/kotlin`, `cli`, `data-pipeline`

This is the **only** normalization logic that exists in more than one place. Every other derived
value — normalized names, phonetic codes, transliterations, search ranks — is computed by the pipeline
and shipped inside the package (see [`ADR-003`](../../docs/adr/ADR-003-search-mechanisms.md)).

Each SDK implements `normalize(_ input: String) -> String` from this document. All implementations
must produce byte-identical output. The oracle is `tests/fixtures/normalization.json`, executed in CI
for every language.

---

## Algorithm

```text
normalize(input):
  1.  Unicode NFKD decomposition
  2.  Strip combining marks            (categories Mn, Mc, Me)
  3.  Special-case substitutions       (§3, applied before case folding)
  4.  Full Unicode case folding        (CaseFolding.txt: F and C, never T)
  5.  Transliterate non-Latin scripts  (pipeline only — §5)
  6.  Replace runs of [^\p{L}\p{N}] with a single space
  7.  Trim
  8.  Collapse internal space runs
```

---

## 1. NFKD decomposition

`NFKD`, not `NFD` and not `NFC`.

- `NFKD` vs `NFC`: compatibility decomposition is required because step 2 removes combining marks, and
  compatibility forms are where most combining marks live (e.g. `Å` → `A` + combining ring).
- `NFKD` vs `NFD`: `ﬁ` (U+FB01 LATIN SMALL LIGATURE FI), `½`, `№` must fold to `fi`, `1 2`, `No`.
  `NFD` leaves them intact and step 6 would mangle them.

Standard normalization is idempotent, so applying NFKD twice is safe.

---

## 2. Combining mark stripping

Remove all characters in Unicode general categories `Mn` (nonspacing), `Mc` (spacing combining) and
`Me` (enclosing).

Applied after NFKD so decomposed marks are visible to the filter. `Mc` is included because spacing
combining marks would otherwise survive step 6 as letters.

---

## 3. Special-case substitutions

Applied in the listed order after mark stripping and before case folding.

**The table must list both cases of every letter.** Case folding runs *after* it, so an uppercase
form that is missing folds down onto its lowercase counterpart and is substituted on the next pass:
`Ærø` → `æro` → `aero`. That breaks idempotence, which the fixture enforces
(`idempotenceCheck`), and idempotence is not cosmetic — the pipeline normalizes a name once when
building `name_norm`, and a consumer may normalize a value it has already read back, or compose
`normalize` with `searchKeyUnaccented`. Three uppercase forms were missing when the Kotlin SDK was
written and the fixture's idempotence check found them: `Æ`, `Ǽ` and `Ə`.

| Input (codepoints) | Output | Reason |
|---|---|---|
| `ß` U+00DF, `ẞ` U+1E9E | `ss` | German; the capital is a distinct code point |
| `æ` U+00E6, `Æ` U+00C6, `ǽ` U+01FD, `Ǽ` U+01FC | `ae` | |
| `œ` U+0153, `Œ` U+0152 | `oe` | |
| `ø` U+00F8, `Ø` U+00D8 | `o` | Danish, Norwegian |
| `đ` U+0111, `Đ` U+0110 | `d` | Croatian, Vietnamese |
| `ð` U+00F0, `Ð` U+00D0 | `d` | Icelandic; distinct from `þ` |
| `þ` U+00FE, `Þ` U+00DE | `th` | Icelandic |
| `ł` U+0142, `Ł` U+0141 | `l` | Polish |
| `ı` U+0131 | `i` | Turkish dotless i, forced together with `i` |
| `İ` U+0130 | `i` | Turkish dotted I (NFKD already removes the combining dot) |
| `ħ` U+0127, `Ħ` U+0126 | `h` | Maltese |
| `ŋ` U+014B, `Ŋ` U+014A | `ng` | |
| `ŧ` U+0167, `Ŧ` U+0166 | `t` | |
| `ə` U+0259, `Ə` U+018F | `e` | Azerbaijani |
| `­` U+00AD | *(removed)* | soft hyphen |
| `‐` U+2010 … `‑` U+2011, `–` U+2013, `—` U+2014, `−` U+2212 | `-` | dashes and minus, then removed by step 6 |
| `’` U+2019, `‘` U+2018, `` ` `` U+0060 | `'` | apostrophe variants, then removed by step 6 |
| ` ` U+00A0 | ` ` | non-breaking space |

**Why the Turkish I entries exist.** §8 of the specification names `Istanbul` / `İstanbul` as a
headline case. Under NFKD, `İ` (U+0130) decomposes to `I` + U+0307 combining dot above; step 2
removes the dot, leaving `I`, which folds to `i`. Meanwhile `ı` (U+0131, dotless) has no decomposition
and would fold to `ı` — a different letter that matches nothing. Both must map to `i` for
`Istanbul` and `İstanbul` to be the same key.

**Why soft hyphen is listed explicitly.** U+00AD survives NFKD and case folding, is invisible, and is
commonly embedded in pasted text from word processors and PDFs. It matches no `LIKE` pattern the user
expects and silently breaks lookups. Removing it is not cosmetic.

---

## 4. Case folding

**Full Unicode case folding**, per `CaseFolding.txt`:

- apply `F` (full) mappings;
- apply `C` (common) mappings;
- **do not apply `T` (Turkic) mappings.**

Turkic mappings are what make `I` fold to `ı` in a Turkish context. Applying them in Turkish and not
in English makes the same input produce different keys in different locales, which breaks
cross-language determinism ([`ARCHITECTURE.md` §8](../../ARCHITECTURE.md#8-cross-language-determinism))
and makes `Istanbul` unfindable from a Turkish device.

This is the specific reason step 4 says *case folding*, not *lowercasing*. Library APIs differ here:
Swift's `lowercased()` is locale-independent but is not case folding, Kotlin's `lowercase()` is
locale-sensitive by default (`lowercase(Locale.ROOT)` required), Python's `str.lower()` is not case
folding, Rust's `to_lowercase()` is not case folding. **None of them implement step 4 as specified.**

Implementations therefore need the mapping data:

- **Swift:** `String.folding(options: [.caseInsensitive], locale: nil)` is the closest available
  primitive; it performs full case folding. Verify against the fixture set — Swift's coverage of
  `CaseFolding.txt` is good but not complete, and any divergence is a CI failure to fix, not to
  paper over.
- **Kotlin:** `String.lowercase(Locale.ROOT)` covers most cases. For full conformance, embed the
  `CaseFolding.txt` table. `Locale.ROOT` is mandatory; the default locale must never be used.
- **Rust:** `str.to_lowercase()` plus a small embedded table for the `F` mappings it does not cover.
- **Python (pipeline):** `str.casefold()` is exactly full case folding. No custom work.

---

## 5. Transliteration — pipeline only

SDKs do **not** transliterate. Cross-script search works through precomputed rows in
`place_alternate_names` (`DATA_MODEL.md` §5.2), so `イスタンブール`, `İstanbul` and `Constantinople`
all reach the same place through indexed lookups.

A consequence worth stating: a query in a script the source did not provide a transliteration for will
not match. For Georgia and Japan, GeoNames supplies transliterations, and recall is measured against
the `GE` and `JP` fixtures before any transliteration table is added. See
[`ARCHITECTURE.md` §13](../../ARCHITECTURE.md#13-open-questions).

---

## 6. Separator handling

Two distinct treatments, and the distinction matters:

**Removed entirely** — apostrophes and quotation marks (`'`, `’`, `‘`, `ʼ`, `` ` ``). `Saint-Denis`
→ `saintdenis`. These are orthographic within a single name, so a space would wrongly split it into
two tokens.

**Replaced with a space** — hyphens, dashes, punctuation, symbols and anything else not a letter or
number: every run of `[^\p{L}\p{N}]` becomes one `U+0020 SPACE`, then trim, then collapse internal
runs. `Saint-Denis` in *German* compound style, or `New-York` → `new york`.

Conflating the two is a real bug: treating apostrophes as separators makes
`D'Angelo` → `d angelo`, and treating hyphens as removable makes `New-York` and `New York` collide
on the same key. They are distinct keys, and both forms are indexed by the pipeline.

Removal of the apostrophe also means `Terra.search("saint denis")` must match `Saint-Denis`. The
pipeline therefore stores an additional alternate-name row for every place whose primary normalized
name contains a space where an apostrophe may have been typed — a cheap, targeted index rather than a
general fuzzy rule.

Punctuation, symbols, emoji and stray marks are all handled by the single run-collapse, so
`İstanbul Cd.`, `İstanbul Cd` and `İstanbul, Cd` all normalize to `istanbul cd`.

---

## 7. Reference vectors

Authoritative test set, mirrored in `tests/fixtures/normalization.json`.

| # | Input | Expected | Covers |
|---|---|---|---|
| 1 | `Istanbul` | `istanbul` | §8 |
| 2 | `İstanbul` | `istanbul` | §8, Turkish dotted I |
| 3 | `istanbul` | `istanbul` | §8 |
| 4 | `ISTANBUL` | `istanbul` | §8 |
| 5 | `Atatürk` | `ataturk` | §8 |
| 6 | `Ataturk` | `ataturk` | §8 |
| 7 | `ATATURK` | `ataturk` | §8 |
| 8 | `Muratpaşa` | `muratpasa` | §4 TR hierarchy |
| 9 | `Türkiye` | `turkiye` | §5 |
| 10 | `Straße` | `strasse` | German ß |
| 11 | `Ağrı` | `agri` | dotless ı |
| 12 | `İzmir` | `izmir` | §16 postal |
| 13 | `Tromsø` | `tromso` | ø |
| 14 | `Łódź` | `lodz` | ł + combining acute |
| 15 | `   Antalya   ` | `antalya` | trim |
| 16 | `San­ta` (U+00AD inside) | `santa` | soft hyphen |
| 17 | `İstanbul Cd.` | `istanbul cd` | punctuation |
| 18 | `Café` | `cafe` | diacritics |
| 19 | `Nice` | `nice` | French cedilla |
| 20 | `Køge` | `koge` | ø |
| 21 | `ﬁnal` (U+FB01) | `final` | NFKD ligature |
| 22 | `①` | `1` | NFKD circled digit |
| 23 | `D'Angelo` | `dangelo` | apostrophe removed |
| 23b | `D’Angelo` | `dangelo` | curly apostrophe removed |
| 24 | `New York` | `new york` | internal space |
| 25 | `New   York` | `new york` | space collapsing |
| 26 | `New-York` | `new york` | hyphen → space |
| 26b | `Saint-Denis` | `saint denis` | hyphen → space, **not** removal |
| 27 | `New-York` | `new york` | hyphen → space |
| 28 | `Saint-Denis` | `saint denis` | hyphen is **not** an apostrophe |
| 29 | `Москва` | `москва` | Cyrillic unchanged — transliteration is pipeline-only |
| 30 | `Tokyo` | `tokyo` | ascii |
| 31 | `東京` | `東京` | unchanged; no Latin form in this policy |
| 32 | `თურქეთი` | `თურქეთი` | unchanged; pipeline supplies transliteration rows |
| 33 | `SW1A 1AA` | `sw1a 1aa` | GB alphanumeric, space preserved |
| 34 | `SW1A1AA` | `sw1a1aa` | GB compact |
| 35 | `07160` | `07160` | leading zero preserved |
| 36 | `İSTANBUL` | `istanbul` | combined Turkish + uppercase |
| 37 | `` (empty) | `` (empty) | empty input |
| 38 | `!!!` | `` (empty) | separators only |
| 39 | `Türkiye Cumhuriyeti` | `turkiye cumhuriyeti` | multi-word |

**Vector 21–22 rationale.** They exist to fail if an implementation uses `NFD` instead of `NFKD`.
`ﬁ` and `①` have no canonical decomposition, so a `NFD` implementation produces `ﬁ` and `①`
unchanged, and the fixture catches it immediately.

**Vector 27–28 rationale.** The pair exists to prevent an implementation from treating hyphens as
apostrophes. `Saint-Denis` → `saint denis` and `D'Angelo` → `dangelo` are different rules, and an
implementation that collapses them either breaks `D'Angelo` or merges two genuinely distinct keys.
A single vector cannot catch this; the pair can.

**Vector 29 rationale.** Cyrillic passes through unchanged because transliteration happens in the
pipeline (§5), not in the SDK. An SDK that transliterates at query time would disagree with
`DATA_MODEL.md` §5.2, and would need the transliteration table in three languages. Georgian (32) and
CJK (31) behave the same way, and both are findable through `place_alternate_names` rows the pipeline
precomputed.

**Vector 33–34 rationale.** Postal codes deliberately preserve internal spaces and leading zeros under
the generic normalizer. Postal codes get a **separate, stricter** normalization defined in
`DATA_MODEL.md` §8 — letters and digits only, no separator collapse — so `SW1A 1AA` and `SW1A1AA` both
reach `sw1a1aa`. Using the generic normalizer on a postal code produces a key that will not match the
indexed value. The stricter function is `postal_normalize`, implemented in the oracle alongside
`normalize`.

---

## 8. Conformance requirements

Each SDK ships a test that:

1. loads `tests/fixtures/normalization.json`;
2. asserts exact byte equality of every vector;
3. asserts `normalize(normalize(x)) == normalize(x)` for all vectors (idempotence);
4. asserts no output contains a combining mark **on a Latin, Greek or Cyrillic base**, and no control
   character. The earlier blanket "no combining marks" invariant was itself wrong: it encoded the
   assumption that section 2 exists to correct, and enforcing it would have forced every
   implementation to destroy Japanese kana;
5. runs as part of `swift test` and `./gradlew test`, with no network access.

A single divergence is a build failure. It is not acceptable to "fix" it by adjusting the fixture:
the fixture is the specification made executable, and changing it to match an implementation
inverts the point.

CI additionally generates randomized inputs from a seeded PRNG over a mixed-script character set and
compares all three implementations, which catches divergences in codepoints no human put in a
hand-written vector.
