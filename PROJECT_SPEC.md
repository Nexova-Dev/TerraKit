# TerraKit

### Universal Geographic Data Package Manager & Offline Geographic Data SDK

---

## 1. Project Overview

Build **TerraKit**, a professional, open-source geographic data package manager and SDK ecosystem for developers.

TerraKit provides structured geographic data for the entire world in a modular, versioned, searchable and offline-first format.

The main idea is:

> Developers should be able to install geographic data packages and access countries, regions, states, provinces, cities, districts, neighborhoods, streets, postal codes, coordinates, time zones, currencies, languages and other geographic metadata directly from their applications.

TerraKit should work especially well with:

- Swift
- iOS
- SwiftUI
- Kotlin
- Android
- Jetpack Compose
- CLI applications
- Backend applications
- REST APIs

The project must be designed as **developer infrastructure**, not simply as a collection of JSON files.

---

# 2. Product Vision

TerraKit should become:

> **The geographic data layer for developers.**

A developer should be able to install geographic data once and use it locally without maintaining their own geographic database.

Example:

```bash
terrakit init
terrakit add TR
```

Then in Swift:

```swift
import TerraKit

let turkey = Terra.country("TR")
let antalya = turkey.city("Antalya")

print(antalya.name)
```

Or Kotlin:

```kotlin
import com.terrakit.core.Terra

val turkey = Terra.country("TR")
val antalya = turkey.city("Antalya")

println(antalya.name)
```

---

# 3. Main Problem

Developers frequently need geographic data for:

- address forms
- delivery applications
- logistics
- hotels
- travel applications
- marketplaces
- e-commerce
- taxi applications
- banking applications
- CRM systems
- registration forms
- location selectors

Developers often have to:

- find different datasets
- normalize them
- build their own database
- maintain country-specific structures
- handle translations
- handle postal codes
- implement search
- update outdated data
- download large datasets
- deal with different geographic hierarchies

TerraKit should solve this problem.

---

# 4. Core Geographic Hierarchy

TerraKit must support hierarchical geographic data.

Basic structure:

```text
World
 └── Country
      └── Region / State / Province
           └── City
                └── District
                     └── Neighborhood
                          └── Street
                               └── Postal Code
```

However, **do not assume every country has the same hierarchy**.

For example:

```text
United States
 └── State
      └── County
           └── City
                └── ZIP Code
```

Türkiye:

```text
Türkiye
 └── Province
      └── District
           └── Neighborhood
                └── Street
                     └── Postal Code
```

Germany:

```text
Germany
 └── State
      └── City
           └── District
                └── Street
```

The data model must support flexible administrative structures.

---

# 5. Core Entities

TerraKit should support at minimum:

```text
Country
Continent
Region
State
Province
County
AdministrativeArea
City
Town
Village
District
Neighborhood
Street
PostalCode
Coordinate
Timezone
Currency
Language
```

Every entity must have a stable unique ID.

Example:

```json
{
  "id": "country_tr",
  "type": "country",
  "name": "Türkiye",
  "iso2": "TR",
  "iso3": "TUR",
  "numericCode": "792"
}
```

City:

```json
{
  "id": "city_tr_antalya",
  "type": "city",
  "name": "Antalya",
  "countryCode": "TR",
  "latitude": 36.8969,
  "longitude": 30.7133,
  "timezone": "Europe/Istanbul"
}
```

---

# 6. Stable IDs

IDs must remain stable between dataset updates whenever possible.

Do not use database auto-increment IDs as the public identity of geographic entities.

Use deterministic or externally stable identifiers where possible.

Example:

```text
country_tr
city_tr_antalya
district_tr_antalya_muratpasa
```

The ID system must be documented.

---

# 7. Localization

TerraKit must support multilingual geographic names.

Example:

```json
{
  "name": {
    "en": "Turkey",
    "tr": "Türkiye",
    "de": "Türkei",
    "ru": "Турция",
    "ka": "თურქეთი"
  }
}
```

Not every entity will have translations.

Implement fallback:

```text
Requested language
       ↓
Localized name
       ↓
English
       ↓
Original/local name
```

Swift:

```swift
country.localizedName(for: .turkish)
```

Kotlin:

```kotlin
country.localizedName(Locale("tr"))
```

Support Unicode correctly.

---

# 8. Search Normalization

Search must handle:

- uppercase/lowercase
- accents
- diacritics
- Unicode variations
- transliteration
- localized names
- alternate names

For example:

```text
Istanbul
İstanbul
istanbul
ISTANBUL
```

should resolve to the same geographic entity where appropriate.

Similarly:

```text
Atatürk
Ataturk
ATATURK
```

should produce relevant results.

---

# 9. Country Packages

Do not require developers to download the entire world.

TerraKit must use modular packages.

Examples:

```bash
terrakit add TR
terrakit add DE
terrakit add GE
terrakit add US
```

Developers should also be able to select different data levels.

Example:

```bash
terrakit add TR --basic
terrakit add TR --cities
terrakit add TR --postal
terrakit add TR --streets
terrakit add TR --full
```

Possible package levels:

```text
basic
administrative
cities
postal
streets
full
```

The exact implementation can be refined during architecture design.

---

# 10. TerraKit CLI

Create a standalone command-line tool:

```bash
terrakit
```

Commands:

```bash
terrakit init
terrakit add TR
terrakit remove TR
terrakit update
terrakit list
terrakit search Antalya
terrakit info TR
terrakit validate
terrakit doctor
terrakit cache
terrakit version
```

Example:

```bash
terrakit init
terrakit add TR
terrakit update
```

The CLI should generate:

```text
terrakit.json
```

Example:

```json
{
  "schemaVersion": 1,
  "packages": {
    "TR": "^1.0.0",
    "DE": "^1.2.0"
  }
}
```

---

# 11. GitHub Package Support

TerraKit must support installing packages from GitHub.

Example:

```bash
terrakit install github.com/example/terrakit-data-tr
```

Support:

```text
Git repository
Git tag
Git branch
Git commit
Semantic version
```

Example:

```bash
terrakit install github.com/NexovaDev/terrakit-data-tr@1.2.0
```

Package integrity must be verified.

---

# 12. TerraKit Registry

Create a central package registry.

The registry should provide package metadata and distribution information.

Example:

```text
GET /packages
GET /packages/{name}
GET /packages/{name}/versions
GET /packages/{name}/latest
GET /countries
GET /search
```

Package metadata:

```json
{
  "name": "terrakit-data-tr",
  "version": "1.0.0",
  "country": "TR",
  "size": 82000000,
  "entities": {
    "cities": 1000,
    "districts": 973,
    "streets": 180000
  }
}
```

---

# 13. Database Architecture

Use a normalized master database for data processing.

Recommended:

```text
PostgreSQL
```

Example tables:

```text
countries
continents
regions
administrative_areas
cities
districts
neighborhoods
streets
postal_codes
coordinates
timezones
languages
currencies
alternate_names
data_sources
dataset_versions
```

Do not store the entire world as one giant JSON document.

---

# 14. Mobile Database

Generated geographic packages should use an efficient embedded database.

Recommended:

```text
SQLite
```

The SDK must query the database instead of loading the entire dataset into memory.

For example:

```swift
Terra.searchStreet("Ataturk")
```

should execute an indexed query.

It must NOT load millions of street records into RAM.

---

# 15. Offline-First Architecture

Offline functionality is a core TerraKit feature.

After installing a dataset:

```swift
Terra.searchCity("Antalya")
```

must work without Internet access.

Same for Kotlin:

```kotlin
Terra.searchCity("Antalya")
```

Network access should only be required for:

- downloading packages
- checking updates
- optional cloud API functionality
- optional remote geocoding
- registry operations

Normal geographic queries should remain offline.

---

# 16. Search Engine

Implement fast indexed search.

Support:

```text
Country
Region
State
Province
City
District
Neighborhood
Street
Postal Code
```

Examples:

```swift
Terra.search("Antalya")

Terra.searchCity("Antalya")

Terra.searchStreet("Ataturk")

Terra.searchPostalCode("07000")
```

Support:

- exact search
- prefix search
- fuzzy search
- localized search
- transliteration
- diacritics normalization
- country filtering
- city filtering
- result ranking

---

# 17. Address Autocomplete

Provide address autocomplete.

Example:

```swift
Terra.autocomplete("Ataturk Cad")
```

Possible result:

```text
Atatürk Caddesi
Muratpaşa
Antalya
Türkiye
07000
```

Return structured results with ranking.

---

# 18. Postal Code System

Support both directions.

Postal code → geographic location:

```swift
Terra.postalCode("07000")
```

Location → postal codes:

```swift
Terra.postalCodes(
    country: "TR",
    city: "Antalya"
)
```

Postal code relationships must support countries where one postal code covers multiple areas.

Do not assume a one-to-one relationship.

---

# 19. Coordinates

Every geographic entity should support optional coordinates where available.

Example:

```swift
Terra.nearest(
    latitude: 36.8969,
    longitude: 30.7133
)
```

Support:

```text
nearest country
nearest city
nearest district
nearest neighborhood
nearest street
nearest postal area
```

Use spatial indexes where appropriate.

---

# 20. Country Metadata

Countries should support:

```text
Name
ISO 3166-1 alpha-2
ISO 3166-1 alpha-3
Numeric country code
Capital
Currency
Phone calling code
Languages
Timezones
Continent
Internet TLD
Driving side
Coordinates
Flag metadata
```

Example:

```swift
let turkey = Terra.country("TR")

turkey.currency
turkey.phoneCode
turkey.languages
turkey.timezones
```

---

# 21. Timezones

Use established timezone standards.

Use:

```text
IANA Time Zone Database
```

Do not create a proprietary timezone system.

Example:

```swift
Terra.city("Antalya").timezone
```

returns:

```text
Europe/Istanbul
```

---

# 22. Currency

Country metadata should reference standardized currency codes.

Example:

```swift
Terra.country("TR").currency
```

returns:

```text
TRY
```

Do not implement currency exchange rates in the core geographic dataset.

That should be a separate module/service.

---

# 23. Swift SDK

Create a native Swift SDK.

Support:

```text
Swift
SwiftUI
UIKit
iOS
iPadOS
macOS
```

Use Swift Package Manager.

Example:

```swift
.package(
    url: "https://github.com/NexovaDev/terrakit-swift.git",
    from: "1.0.0"
)
```

Usage:

```swift
import TerraKit

let countries = Terra.countries

let turkey = Terra.country("TR")

let cities = turkey.cities

let antalya = turkey.city("Antalya")

let districts = antalya.districts
```

Use modern Swift concurrency where appropriate.

Keep dependencies minimal.

---

# 24. Kotlin SDK

Create a native Kotlin SDK.

Support:

```text
Kotlin
Android
Jetpack Compose
JVM where practical
```

Gradle:

```kotlin
implementation("com.terrakit:geo-core:1.0.0")
```

Usage:

```kotlin
import com.terrakit.core.Terra

val turkey = Terra.country("TR")

val antalya = turkey.city("Antalya")

val districts = antalya.districts
```

Use Kotlin coroutines where asynchronous work is required.

---

# 25. Android Integration

The Android SDK should use an optimized local database.

Consider:

```text
SQLite
Room
```

Use proper indexes for:

```text
city names
district names
street names
postal codes
country codes
coordinates
```

Avoid loading large datasets into RAM.

---

# 26. iOS Integration

The Swift SDK should use an efficient embedded database.

SQLite is preferred unless another solution provides a clearly superior architecture.

Support:

```text
lazy loading
pagination
indexed queries
caching
streaming where useful
```

---

# 27. Data Pipeline

Build a dedicated data ingestion and processing pipeline.

Architecture:

```text
External Data Sources
        ↓
Downloader
        ↓
Parser
        ↓
Normalizer
        ↓
Deduplicator
        ↓
Hierarchy Builder
        ↓
Validator
        ↓
Database Generator
        ↓
SQLite Generator
        ↓
Package Builder
        ↓
Checksum / Signature
        ↓
Registry
```

The pipeline must be reproducible.

---

# 28. Geographic Data Sources

Use only legally usable data.

Potential sources include:

```text
OpenStreetMap
GeoNames
Official government datasets
Official postal datasets where licensing allows
ISO standards
IANA timezone database
Other appropriately licensed datasets
```

Do not copy proprietary geographic databases without permission.

Every dataset must contain:

```text
source
sourceVersion
license
attribution
generatedAt
```

---

# 29. Licensing and Attribution

Licensing is a first-class feature.

Every package must declare:

```json
{
  "license": "...",
  "sources": [],
  "attribution": []
}
```

The documentation must clearly explain:

- data source
- source license
- redistribution rights
- attribution requirements
- modifications
- update policy

The project must never hide source attribution.

---

# 30. Data Validation

Implement automated validation.

Check for:

```text
duplicate IDs
duplicate names
missing parent IDs
broken hierarchy
invalid country codes
invalid coordinates
invalid postal codes
invalid Unicode
missing required fields
orphaned entities
```

CLI:

```bash
terrakit validate TR
```

Output:

```text
✓ Country structure
✓ Regions
✓ Cities
✓ Districts
✓ Neighborhoods
✓ Streets
✓ Postal codes
✓ Coordinates

Errors: 0
Warnings: 17
```

---

# 31. Versioning

Use semantic versioning for software packages:

```text
MAJOR.MINOR.PATCH
```

Geographic datasets should have their own version.

Example:

```text
TerraKit Swift SDK: 1.4.0
TerraKit Kotlin SDK: 1.4.0
Türkiye Dataset: 2026.10.1
Germany Dataset: 2026.09.4
```

Keep SDK versions independent from geographic dataset versions whenever possible.

---

# 32. Dataset Updates

Allow developers to check for dataset updates.

Example:

```swift
Terra.updateAvailable()
```

or:

```kotlin
Terra.updateAvailable()
```

Updates should be:

- optional
- versioned
- verifiable
- resumable where possible
- backward compatible

Do not force network access into normal offline queries.

---

# 33. Delta Updates

Large datasets may make full downloads inefficient.

Eventually support:

```text
full package
delta package
```

Example:

```text
Turkey v2026.10.0
       ↓
Turkey v2026.10.1

Full package: 100 MB
Delta: 2.5 MB
```

This should be implemented after the MVP.

---

# 34. Package Compression

Use efficient compression.

Consider:

```text
Zstandard
GZIP
ZIP
```

Package generation should optimize:

```text
download size
installation speed
disk usage
database performance
```

---

# 35. Package Integrity

All packages must have checksums.

Use:

```text
SHA-256
```

Optionally support cryptographic signatures.

Installation flow:

```text
Download
   ↓
Checksum verification
   ↓
Signature verification
   ↓
Manifest validation
   ↓
Installation
```

Example:

```text
Downloading geo-data-tr...
Checking SHA-256...
Checking signature...
Installing...
✓ Verified
```

---

# 36. Address Model

Create a standardized address object.

Example:

```json
{
  "country": {
    "code": "TR",
    "name": "Türkiye"
  },
  "city": {
    "id": "antalya",
    "name": "Antalya"
  },
  "district": {
    "id": "muratpasa",
    "name": "Muratpaşa"
  },
  "neighborhood": {
    "name": "Fener"
  },
  "street": {
    "name": "Atatürk Caddesi"
  },
  "postalCode": "07160",
  "latitude": 36.85,
  "longitude": 30.75
}
```

The model must allow missing fields because not every country provides every level.

---

# 37. REST API

Create an optional hosted API.

Endpoints:

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

Implement:

```text
API versioning
API keys
rate limiting
authentication
caching
request logging
```

The hosted API is optional functionality and must not be required for the core offline SDK.

---

# 38. Admin Dashboard

Create an optional web dashboard for TerraKit administrators.

Sections:

```text
Dashboard
Countries
Regions
Cities
Districts
Neighborhoods
Streets
Postal Codes
Data Sources
Dataset Versions
Validation
Packages
Registry
API Usage
System Logs
```

The dashboard must include an audit trail.

Do not allow uncontrolled manual modification of authoritative geographic data.

---

# 39. Security

Implement:

```text
HTTPS
package checksums
package signatures
API key protection
rate limiting
authentication
registry security
dependency scanning
secure update mechanism
```

Never execute arbitrary package code during installation.

Packages should primarily contain validated data and metadata.

---

# 40. Performance

TerraKit must be designed for millions of geographic records.

Initial performance targets:

```text
Simple entity lookup: <10 ms
Indexed search: <50 ms
Autocomplete: <100 ms
```

These are benchmark targets, not hard guarantees.

Create benchmarks for:

```text
country lookup
city lookup
district lookup
street lookup
postal lookup
autocomplete
fuzzy search
coordinate lookup
nearest location
database initialization
package installation
dataset update
```

---

# 41. Memory Efficiency

Never load entire datasets into memory.

Avoid:

```swift
let allStreets = database.loadEverything()
```

Prefer:

```swift
Terra.searchStreet("Ataturk")
```

with:

```text
indexes
pagination
lazy loading
caching
query limits
```

---

# 42. Repository Architecture

Use a clean monorepo initially:

```text
terrakit/
│
├── core/
│
├── cli/
│
├── registry/
│
├── api/
│
├── data-pipeline/
│
├── database/
│
├── schemas/
│
├── datasets/
│   ├── tr/
│   ├── ge/
│   ├── de/
│   ├── us/
│   └── gb/
│
├── sdk/
│   ├── swift/
│   └── kotlin/
│
├── docs/
│
├── examples/
│
├── tests/
│
├── docker/
│
├── README.md
├── LICENSE
├── CONTRIBUTING.md
└── SECURITY.md
```

This structure can be changed if the architecture requires it.

---

# 43. Recommended Technology Stack

## Backend

```text
Node.js
TypeScript
PostgreSQL
Redis
REST API
Docker
```

## Data Pipeline

```text
Python
PostgreSQL
GDAL/geospatial tooling where appropriate
```

## CLI

Prefer a compiled standalone CLI.

Recommended:

```text
Rust
```

The CLI should be distributed as a single executable for:

```text
macOS
Linux
Windows
```

## Swift

```text
Swift
Swift Package Manager
SQLite
Swift Concurrency
```

## Kotlin

```text
Kotlin
Gradle
Android
SQLite / Room
Kotlin Coroutines
```

## Infrastructure

```text
Linux
Docker
PostgreSQL
Object Storage
CDN
```

---

# 44. Future SDKs

The core data format must remain language-independent.

Future SDKs may include:

```text
JavaScript
TypeScript
Python
Dart
Rust
Go
PHP
Ruby
```

Do not design the data model specifically around Swift or Kotlin.

---

# 45. Future Geographic Modules

The architecture should eventually allow:

```text
Airports
Railway Stations
Bus Stations
Ports
Universities
Hospitals
Hotels
Tourist Attractions
Postal Offices
Banks
ATMs
Government Offices
Emergency Services
Roads
Boundaries
Points of Interest
```

These should be separate modules rather than forcing everything into the core package.

---

# 46. Future Geospatial Features

Possible future products/modules:

```text
Geocoding
Reverse Geocoding
Routing
Distance calculation
Map data
Geofencing
Boundary polygons
Nearby search
Map tiles
```

Do not implement these in the initial MVP unless required.

---

# 47. Open Source Strategy

TerraKit should be designed as an open-source project.

Open-source components:

```text
TerraKit Core
TerraKit CLI
Swift SDK
Kotlin SDK
Data schemas
Data pipeline
Documentation
```

Potential hosted services:

```text
TerraKit Registry
TerraKit Cloud API
Private packages
Enterprise datasets
Managed updates
Analytics
```

Do not artificially cripple the open-source core.

---

# 48. MVP Scope

Do not attempt to build every country immediately.

Start with:

```text
Türkiye
Georgia
Germany
United States
United Kingdom
```

Core entities:

```text
Country
Region
City
District
Neighborhood
Street
Postal Code
Coordinate
```

Core tools:

```text
CLI
Swift SDK
Kotlin SDK
Package Registry
SQLite
PostgreSQL
Search
Autocomplete
Validation
Versioning
```

---

# 49. MVP CLI

The following commands must work:

```bash
terrakit init

terrakit add TR

terrakit remove TR

terrakit update

terrakit list

terrakit search Antalya

terrakit info TR

terrakit validate TR

terrakit doctor

terrakit version
```

---

# 50. MVP Swift API

Provide:

```swift
import TerraKit

Terra.countries

Terra.country("TR")

Terra.city("Antalya")

Terra.search("Antalya")

Terra.searchCity("Antalya")

Terra.searchStreet("Ataturk")

Terra.searchPostalCode("07000")

Terra.autocomplete("Ataturk Cad")

Terra.nearest(
    latitude: 36.8969,
    longitude: 30.7133
)
```

---

# 51. MVP Kotlin API

Provide equivalent functionality:

```kotlin
import com.terrakit.core.Terra

Terra.countries

Terra.country("TR")

Terra.city("Antalya")

Terra.search("Antalya")

Terra.searchCity("Antalya")

Terra.searchStreet("Ataturk")

Terra.searchPostalCode("07000")

Terra.autocomplete("Ataturk Cad")

Terra.nearest(
    36.8969,
    30.7133
)
```

---

# 52. Testing

Implement:

```text
Unit tests
Integration tests
CLI tests
Database tests
SDK tests
Search tests
Autocomplete tests
Package tests
Validation tests
Update tests
Performance tests
```

Use different geographic structures in testing.

At minimum:

```text
Türkiye
Georgia
Germany
United States
United Kingdom
Japan
```

Japan should be included because it demonstrates that geographic/address structures can differ substantially from Western models.

---

# 53. Documentation

Create professional documentation:

```text
Introduction
Getting Started
Installation
CLI
Swift
Kotlin
Data Model
Packages
Registry
Search
Autocomplete
Postal Codes
Coordinates
Localization
Offline Mode
Updates
Versioning
Data Sources
Licensing
API
Architecture
Security
Contributing
FAQ
Roadmap
```

Every public SDK API must have documentation and examples.

---

# 54. Project Documentation Files

Before writing major implementation code, create:

```text
ARCHITECTURE.md
DATA_MODEL.md
PACKAGE_FORMAT.md
CLI_SPEC.md
SDK_SPEC.md
API_SPEC.md
DATA_PIPELINE.md
SECURITY.md
LICENSING.md
ROADMAP.md
CONTRIBUTING.md
```

---

# 55. Development Methodology

You are acting as the **lead architect and senior engineer** for TerraKit.

Do not immediately start writing thousands of lines of code.

First:

1. Analyze the requirements.
2. Identify contradictions.
3. Identify missing requirements.
4. Identify technical risks.
5. Propose architectural improvements.
6. Explain important trade-offs.
7. Design the data model.
8. Design the package format.
9. Design the registry.
10. Design the SDK interfaces.
11. Define the MVP.
12. Create the implementation roadmap.
13. Then begin implementation.

If the specification contains a technically inferior approach, do not blindly follow it.

Explain the issue and propose a better solution.

---

# 56. Engineering Principles

Follow these principles:

1. Offline-first.
2. Modular datasets.
3. Small memory footprint.
4. Fast indexed queries.
5. Stable geographic IDs.
6. Version everything.
7. Never assume every country has the same hierarchy.
8. Separate data from SDK logic.
9. Separate raw source data from normalized data.
10. Respect all dataset licenses.
11. Reproducible builds.
12. Secure package verification.
13. Backward-compatible schemas.
14. Minimal dependencies.
15. Excellent developer experience.
16. Design for millions of records.
17. Never load huge datasets into memory.
18. Automate data validation.
19. Make the architecture extensible.
20. Build the MVP before advanced features.

---

# 57. Final Developer Experience

The final experience should be extremely simple.

Install:

```bash
terrakit init
terrakit add TR
```

Swift:

```swift
import TerraKit

let city = Terra.city("Antalya")

print(city.name)
print(city.districts)
```

Kotlin:

```kotlin
import com.terrakit.core.Terra

val city = Terra.city("Antalya")

println(city.name)
println(city.districts)
```

The developer should not need to:

- find geographic datasets
- normalize geographic data
- create their own database
- implement address search
- manually maintain postal codes
- manually handle country hierarchies
- download the entire world
- depend on an Internet connection for basic geographic queries

---

# 58. Long-Term Vision

TerraKit should eventually become a global geographic infrastructure ecosystem.

Conceptually:

```text
                         TerraKit
                            │
       ┌────────────────────┼────────────────────┐
       │                    │                    │
     Data                  SDKs                Tools
       │                    │                    │
 Countries               Swift               CLI
 Cities                  Kotlin              Registry
 Streets                 Python              API
 Postal Codes            JS                  Dashboard
 Coordinates             Dart
 Timezones
 Languages
 Currencies
       │
       └─────────────── Geographic Layer ───────────────┐
                                                        │
                         Applications                   │
                                                        │
          ┌───────────────┬───────────────┬────────────┤
          │               │               │
       Delivery         Hotels         Marketplace
          │               │               │
       Logistics        Travel          E-commerce
```

The ultimate goal is:

> **TerraKit becomes the standard geographic data layer developers can install, query and use offline across multiple programming languages and platforms.**

Build the system carefully from the MVP upward. Prioritize correctness, licensing, performance, developer experience and maintainability over the number of features.