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

## Not used

`alternateNamesV2.zip` (~200 MB) is what would supply localized names, exonyms
and transliterations — the content behind `place_names` and
`place_alternate_names` in [`DATA_MODEL.md` §5](../../DATA_MODEL.md#5-names-localization-and-normalization).
Those tables are in the schema and validated, but the ingestion step is not
written yet, so search currently relies on `places.name` alone. This is the next
pipeline stage and the biggest remaining gap.
