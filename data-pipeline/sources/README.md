# GeoNames — pinned source definitions

Every TerraKit package is built from these files. The checksums are recorded in
`provenance.json` inside the cache, and a changed checksum is reported as a
warning so an upstream correction is noticed rather than silently consumed.

## Files used

| Key | URL | Used for |
|---|---|---|
| `countryInfo` | `countryInfo.txt` | ISO 3166-1 metadata, capital, currency, languages, postal regex |
| `admin1` | `admin1CodesASCII.txt` | level-2 administrative areas; codes are ISO 3166-2 |
| `admin2` | `admin2Codes.txt` | level-3 administrative areas, keyed by geonameid |
| `cities500` | `cities500.zip` → `cities500.txt` | settlements of 500+, with population and IANA timezone |
| `postal` | `{ISO2}.zip` → `{ISO2}.txt` | postal codes and their areas — **not published for every country** |
| `alternateNames` | `alternateNamesV2.zip` → `alternateNamesV2.txt` | localized names, exonyms, historic names, transliterations |

Base URL: `https://download.geonames.org/export/`

## Licensing

**GeoNames is CC BY 4.0.** Redistribution and commercial use are permitted;
attribution is required. There is no share-alike obligation, which is the reason
GeoNames is the primary source and OSM is not
([`ADR-004`](../../docs/adr/ADR-004-geonames-first-osm-later.md)).

Required notice, emitted into every package manifest and returned by
`Terra.attribution`:

```
Contains data from GeoNames (https://www.geonames.org/), CC BY 4.0.
```

Any dataset added to this file must carry a resolved license before the pipeline
will emit a package containing it. That check has no override flag
([`LICENSING.md` §6](../../LICENSING.md#6-pipeline-enforcement)).

## Known upstream gaps

Found while building the MVP countries. Recorded because each one produces a
warning or a fallback that a maintainer will otherwise re-discover.

**No postal file for Georgia.** `GE.zip` returns HTTP 404. Georgia is an MVP
country (spec §48), so the postal source is marked *optional*: the downloader
treats a 404 as "this dataset does not exist" rather than a failure, and the
build demotes to `level: "cities"`. The manifest declares the real level, so the
SDK reports the postal feature unavailable instead of returning empty results
that look like the country has no postal codes.

**`admin1_code` of `"00"` in Georgia.** Three settlements (Znaur, Bagdadi,
Merkheuli) reference admin1 `00`, which is absent from `admin1CodesASCII.txt`.
They attach to the country root and their level drops to 2, because the parent
is the only honest description of where they sit. They are warned about, not
dropped: a user searching for Znaur should find something.

**`Asia/Baghdad` for one Turkish settlement.** Geonameid 97473 (Çey, Van) carries
`Asia/Baghdad`. Preserved as published rather than corrected, because silently
overriding upstream data hides the fact that it is wrong. Worth a data-quality
issue upstream.

**`admin2_code` is a geonameid, `admin1_code` is not.** In `cities500.txt`,
`admin1_code` is the short numeric admin code (`07`) while `admin2_code` and
`admin3_code` are geonameids (`7732185`). In `{ISO2}.txt` the same asymmetry
appears. This is not a bug and not documented by GeoNames; the readers assert
the column count and the hierarchy builder resolves the two key spaces
explicitly. Anyone extending the pipeline will hit it.

**`countryInfo.txt` has 19 columns, not 47.** Documented upstream, easy to guess
wrong. The readers assert the count so a future format change fails loudly.

**Country names are English.** Column 5 gives `Turkey`, not `Türkiye`. The
endonym arrives through `alternateNames`, which is not yet ingested. See
[`ARCHITECTURE.md` §13](../../ARCHITECTURE.md#13-open-questions).

**No postal file for the United Kingdom.** `GB.zip` is served but empty of usable
areas, and the build demotes to `level: "cities"` exactly as it does for Georgia.
GB is the second country in the MVP set with this shape, which is worth knowing
before a consumer asks why two of six countries answer `postalCode` with nothing.
The manifest omits the `postal` feature, so `Terra.postalCodes(country: "GB")`
returns an empty list and the CLI says so in JSON rather than staying silent.

**Two British settlements have a latitude and no longitude.** Geonameid 2636714
(Stratford) and 2637528 (Somersham) carry `51.53333`/`52.38333` with an empty
longitude. They are absent from `places_rtree`, correctly: an R*Tree entry needs a
box and half a point is not one. The validator reports them as a
`partial-coordinates` warning rather than absorbing them, because the alternative
is that "places with coordinates" quietly changes meaning from "latitude and
longitude" to "latitude".

**GeoNames' own names for the US and UK are `USA` and `UK`.** `countryInfo.txt`
column 5 is the promoted display name, so `country("US").name` is `USA` and
`country("GB").name` is `UK` — not `United States` and `United Kingdom`. Reported
faithfully rather than curated, because the pipeline's job is to ingest what the
source says and inventing a name is inventing data. A consumer that needs the
long form has `localizedName(for: .english)`. Worth raising upstream: both are
abbreviations where an address UI wants a name a person would write on an envelope.

## alternateNamesV2 — the largest source, and the most dangerous

`alternateNamesV2.zip` is **205 MB compressed, 785 MB of text, 19 219 334 rows**, and it holds
every alternate name for every country and language on Earth. The archive also contains
`iso-languagecodes.txt`, which is the authoritative list of ISO 639 codes and is used to tell a real
language from a sentinel.

Every value of `isolanguage` was measured against that list rather than guessed:

| Category | Examples | Count | Handling |
|---|---|---|---|
| no language | *(empty)* | 7 343 131 | dropped unless the name is historic |
| identifier | `wkdt` (Wikidata), `link` (URL), `post`, `unlc`, `lauc`, `iata`, `uicn`, `geoid`, `nuts` | ~2.2 M | dropped — not names |
| **bibliographic ISO 639-2** | `ger`, `fre`, `dut`, `cze`, `geo`, `chi`, `gre`, `alb` | small | **remapped** to `de`, `fr`, `nl`, `cs`, `ka`, `zh`, `el`, `sq` |
| BCP-47 with script/region | `zh-Hans`, `zh-Hant`, `pt-BR`, `en-GB`, `de-CH` | ~20 000 | kept unchanged |
| real languages | 653 distinct, `zh` and `en` largest | ~13 M | kept |

Three findings worth keeping.

**Two identifier classes are marked `isPreferredName` upstream.** The real file contains
`link | https://en.wikipedia.org/wiki/Istanbul | isPreferred=1`. A naive ingest can therefore put a
Wikipedia URL in a name column. This is why `IDENTIFIER_CLASSES` exists and why the `link` mapping
is checked before any preferred-name logic.

**Bibliographic codes are not junk.** `ger`, `fre`, `dut`, `geo`, `chi` and 13 more are absent from
`iso-languagecodes.txt`, so a validity check that only consults that file classifies them as garbage
and discards them. They are real languages. Without the remapping, every German name GeoNames tags
`ger` is dropped and `Terra.localizedName(for: .german)` returns the English name.

**Roughly 70% of a country's rows are unverified user submissions.** For Türkiye, 13 088 of 34 728
rows were dropped for having no language and no discipline flag — misspellings ("Antaliyah",
"Antayla", "Istambul") and bare Wikipedia links. Shipping those would make `search("Istanbul")`
match a typo as though it were a distinct place, and would put unverified data in an
authoritative-looking table.

### Column layout

```text
1 alternateNameId   2 geonameid   3 isolanguage   4 alternate_name
5 isPreferredName   6 isShortName  7 isColloquial  8 isHistoric
9 from              10 to
```

`from` and `to` are validity dates, evaluated against the pipeline run's epoch and never the wall
clock — a build whose content depends on today's date is not reproducible.

### Reading cost

One pass, 8.2 s. The geonameid is extracted from the raw bytes and tested against the country's set
*before* any UTF-8 decode, because decoding 19 million lines to find one field is minutes of work
for no benefit.
