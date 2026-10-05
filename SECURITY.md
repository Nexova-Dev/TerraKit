# SECURITY

Status: **Draft**
Last updated: 2026-10-05

Covers package integrity and signing, the trust boundary around GitHub sources, and the SDK's
attack surface. See [`PACKAGE_FORMAT.md` §5](PACKAGE_FORMAT.md#5-integrity) for the verification
order.

---

## 1. Threat model

### Assets

| Asset | Why it matters |
|---|---|
| Package integrity | Malicious data in an address form reaches millions of users |
| Release signing key | Full compromise means arbitrary packages install everywhere |
| Geographic correctness | Wrong postal codes cause failed deliveries and financial errors |
| Source license compliance | Legal exposure for downstream redistributors |
| User privacy | Location data is sensitive |

### Adversaries

| Adversary | Capability |
|---|---|
| Network MITM | Read and alter registry traffic |
| Malicious package author | Publishes a package signed with a compromised or stolen key |
| Hostile registry operator | Serves a different manifest than the one users expect |
| Compromised mirror or CDN | Alters artifacts in transit or at rest |
| Malicious contributor | Submits data with a hidden license obligation or a poisoned normalization vector |
| Local attacker | Reads or replaces files in `.terrakit/` on a shared machine |

### Out of scope

Compromised OS, malicious application code running in the same process, and side-channel attacks on
the device. The SDK is a data-reading library and cannot defend against an attacker already executing
code as the user.

---

## 2. Trust model

```text
  TerraKit release key  ──signs──▶  manifest.json  ──carries──▶  sha256 of data.sqlite
         (offline)                        (verified first)            (verified second)

  Trust root: the public keyring compiled into the SDK.
  Everything else is data derived from that keyring.
```

The SDK trusts exactly one thing: its compiled-in Ed25519 keyring. Not the registry, not the CDN, not
GitHub, not a file in the project directory.

There is deliberately **no TOFU** (trust on first use) and **no fallback to unsigned packages**. A
keyring that can be extended by whatever arrives first is not a trust root, and a keyring that
degrades to "accept anything" provides no protection at all.

---

## 3. Package verification

Order, from [`PACKAGE_FORMAT.md` §5.1](PACKAGE_FORMAT.md#51-verification-order):

```text
1. Download manifest
2. Verify Ed25519 signature over canonical manifest bytes
3. Verify SHA-256 of each file listed in manifest.files[]
4. Verify manifest.packageSha256
5. Check schemaVersion against the SDK's supported range
6. Extract into a temp directory
7. Re-verify SHA-256 on the extracted file
8. Atomic rename into place
```

Four points where this is more than routine:

**The signature covers canonical bytes, not a parsed object.** Signing the deserialized manifest is
how JSON signature confusion bugs get introduced: a second parser can read the same bytes as a
different document. The signature is checked before any JSON parsing.

**Nothing is trusted before step 2.** Not the size, not the archive listing, not the filename.
A package failing step 2 is deleted rather than kept for inspection, because keeping it invites
"let me look at why it failed".

**Verification runs again after extraction** (step 7). A decompression bug, a truncated stream or a
hostile `.zst` can produce bytes that never matched the download. Verifying only the compressed
artifact verifies nothing about what actually lands on disk.

**Failure is closed.** Invalid signature, unknown key and expired key all fail identically, all exit
non-zero, and none install. There is no `--allow-unverified` that changes what is installed, only
whether the user is warned — and it refuses to run in CI.

---

## 4. Signature scheme

| Property | Choice | Reason |
|---|---|---|
| Algorithm | Ed25519 | 64-byte signatures, no parameters to negotiate, no certificate chain |
| Signed content | canonical `manifest.json` bytes | see above |
| Key ID | `keyId` string in `signature.json` | supports rotation |
| Key storage | offline, hardware-backed where available | limits blast radius |

### 4.1 Key rotation

The SDK keyring maps `keyId` → public key.

1. A new key is generated and used for signing new releases.
2. A new SDK release ships a keyring containing **both** keys.
3. The old key stays trusted until every package it signed is outside the support window.
4. Removal of the old key happens in a subsequent SDK release.

Old keys are never removed in the same release that stops using them — a user who skips an SDK
release must not end up unable to verify packages they already have.

### 4.2 Compromise

Signing key compromise is the scenario that matters most, because every user trusts it implicitly.

Response, in order:

1. **Publish a revocation list** signed by a still-trusted key. This is why rotation keeps the old key
   trusted for a window: there must be a key left that can revoke the compromised one.
2. **Ship an SDK release** adding the revocation. Verification fails closed for revoked keys.
3. **Rotate to a new key** and re-sign the affected packages.
4. **Publish an advisory** with the affected version range.

The revocation list is a signed file fetched by SDK update and shipped in the SDK itself. An SDK that
cannot reach the network still has the shipped list, which is the common case since packages are
verified offline.

**Limitation, stated plainly:** a compromised key cannot retroactively protect users who installed a
malicious package during the exposure window. Signatures prove a package came from a given key; they
do not prove its contents are correct. Package contents are additionally constrained by schema
validation, which rejects structurally malformed data regardless of signature validity.

---

## 5. GitHub sources

§11 requires `terrakit install github.com/example/terrakit-data-tr`. GitHub is an untrusted transport,
not a trusted publisher.

- Signature verification applies exactly as for registry packages. A valid TerraKit signature
  installs; an untrusted one does not.
- The CLI verifies the commit SHA it resolved against the one it checked out, then verifies the
  package SHA-256 against its own manifest. A manifest cannot claim a checksum its files do not have.
- Branch and commit references resolve to a concrete SHA, pinned in `terrakit.json` and recorded in
  the manifest as `resolvedRef`.
- `--allow-unsigned` exists for local development. It prints a warning on every invocation and
  **refuses to run when CI is detected**.

The reasoning: if anyone can push to a branch, anyone can push data. The only thing between that and
a production application is signature verification. Treating a GitHub URL as a trust signal would
make the entire integrity model decorative.

### 5.1 No code execution

§39 forbids executing package code during installation. A package contains:

- `manifest.json` — declarative metadata
- `data.sqlite` — data, validated against a fixed schema
- `signature.json` — signature metadata

There is no hook, no script, no native library, no dynamic SQL, no loadable SQLite extension, and no
`CREATE TRIGGER`. The installer opens the SQLite file with the default flags — no
`SQLITE_OPEN_URI`, no `enable_load_extension`, no `trusted_schema=0` violations. A database that
contains a trigger is a validation error.

This is enforced by an allowlist on package contents: **anything not on that list is an error, not a
warning.** An unknown file in a package means either a corrupt download or a supply-chain attack, and
both deserve the same response.

---

## 6. SDK attack surface

The SDK is deliberately small, because a data-reading library should be.

| Property | State |
|---|---|
| Third-party dependencies | **none** beyond `libsqlite3` |
| Network access | none in any query path; `updateAvailable()` is opt-in |
| Filesystem writes | none; the database is opened read-only |
| Dynamic SQL | none; every query is a prepared statement with bound parameters |
| SQLite extensions | none loaded, ever |
| Subprocess execution | none |

Read-only is enforced at the file level, not by convention: the SDK opens with
`SQLITE_OPEN_READONLY`. A tampered package is still user-data on their machine, and read-only mode
means a compromised SDK cannot escalate to writing.

All user-supplied strings reach SQLite exclusively as bound parameters. String interpolation into
SQL is not used anywhere, because `name_norm` values contain arbitrary Unicode from upstream data and
a single missed quote is a full injection.

---

## 7. Local filesystem

`.terrakit/` is user-writable, so a local attacker with write access can replace a package. Mitigations
already in place:

- SHA-256 is recorded in the local `terrakit.json` lockfile, so `terra validate` detects tampering
  after the fact.
- Packages are immutable by convention; the installer never rewrites an existing version directory.
- `.terrakit/cache/` is shared across projects and is the widest blast radius. It is rebuilt
  on `terrakit cache clean`, and deleting it is always safe.

Not addressed, and honestly so: on a shared machine another user with write access to `.terrakit/`
can serve tampered data, and nothing in a userspace library can prevent that. Signing covers the
supply chain; it does not cover local tampering by someone who already has your account.

---

## 8. Dependency policy

§56.14 requires minimal dependencies. Concretely:

- **SDKs:** zero third-party dependencies. `libsqlite3` plus the standard library.
- **CLI:** audited RustCrypto crates for Ed25519 and SHA-256. Hand-rolling either would be a serious
  defect; verification correctness is not an area for improvising.
- **Pipeline:** pinned versions with hashes, installed from a lockfile.
- **CI:** dependency scanning on every build, with automated update PRs for patch releases.

Adding a dependency to an SDK requires a documented justification in `docs/adr/` that addresses
binary size, transitive dependency count and whether the functionality could reasonably be maintained
in-tree. The default answer is no.

---

## 9. Reporting a vulnerability

Security issues are handled privately, not through public issues.

- Report via GitHub Security Advisories on the repository, or by email to the maintainers.
- Acknowledgement within 3 business days.
- Assessment within 10 business days.
- Fix or mitigation plan within 30 days, with a coordinated disclosure date agreed with the reporter.
- Credit the reporter unless they prefer otherwise.

**Reporting a data-quality problem in a package is not a security report.** Wrong postal codes and
broken hierarchy are bugs — open an issue. Reserve private reporting for integrity, verification and
execution vulnerabilities.
