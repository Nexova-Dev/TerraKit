# TerraKit API Specification

Status: **Stub — not written**
Last updated: 2026-10-05

The hosted API is **optional functionality** and must never be required by the
core offline SDK (SPEC §37). Nothing in this project blocks on it.

---

## What is already decided

These do not need revisiting when the API is written.

| Decision | Where |
|---|---|
| The **registry is static files**. `index.json` plus per-package manifests on object storage behind a CDN. No database, no application server. | [`PACKAGE_FORMAT.md` §11](PACKAGE_FORMAT.md#11-registry), [`ADR-007`](docs/adr/ADR-007-language-stack.md) |
| A dynamic API is needed **only** when API keys and rate limiting appear, and it reads from the same object storage. | [`ARCHITECTURE.md` §12](ARCHITECTURE.md#12-where-to-start-contributing) |
| The API is **read-only over published packages**. It never writes data. | [`ARCHITECTURE.md` §7](ARCHITECTURE.md#7-module-boundaries) |
| Language: TypeScript, per SPEC §43. | [`ADR-007`](docs/adr/ADR-007-language-stack.md) |
| PostgreSQL and Redis are listed in SPEC §43 and are **not needed** until the API has state. Adding them at the start means operating two services to serve static JSON. | [`ADR-007`](docs/adr/ADR-007-language-stack.md) |

## Endpoints

Named in SPEC §37, unstarted:

```text
GET /v1/countries
GET /v1/countries/{code}
GET /v1/countries/{code}/cities
GET /v1/cities/{id}
GET /v1/search
GET /v1/autocomplete
GET /v1/postal-codes/{code}
GET /v1/nearby
```

## Required features

Per SPEC §37: API versioning, API keys, rate limiting, authentication, caching,
request logging.

## Design notes to carry forward

- **`/v1/search` has no static equivalent.** Every other read path is a CDN object;
  search needs the SQLite files on a server and a query planner. That asymmetry is
  the entire reason a dynamic API exists at all, and it is worth remembering when
  deciding how much infrastructure to stand up.
- **IDs are opaque in responses too.** Returning `countryCode` and `level` as
  fields is what lets a client avoid parsing them
  ([`SDK_SPEC.md` §13](SDK_SPEC.md#13-anti-patterns)).
- **Attribution belongs in every response** that contains geographic data. A client
  consuming the API has the same CC BY obligation as one consuming a local package,
  and burying the notice in a docs page means nobody reads it
  ([`LICENSING.md` §5](LICENSING.md#5-required-cc-by-attribution-notice)).
- **Rate limiting per API key, not per IP**, once keys exist. Per-IP breaks any
  application behind a shared NAT, which is most mobile apps.

## Open questions

1. **Whether the API serves data itself or proxies to a hosted SDK.** Serving data is
   faster and cheaper; proxying guarantees clients and API agree on semantics. Both are
   defensible and the choice depends on hosting cost, which is not yet known.
2. **Self-hosting.** Whether a user can run the same API against their own private
   packages. The per-package license model already supports private packages, so this
   is a deployment question rather than a data-model one.
3. **Analytics.** SPEC §47 lists analytics as a hosted service. Worth deciding early
   what is and is not collected, since a geographic API can be deanonymizing if it logs
   raw queries.
