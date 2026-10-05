#!/usr/bin/env python3
"""
Build and publish the static registry.

The registry is files, not a service (`PACKAGE_FORMAT.md` §11). This script produces that file tree
and optionally syncs it to object storage:

```text
index.json                                 all packages, latest version of each
packages/{name}.json                       all versions of one package
packages/{name}/{version}/manifest.json
packages/{name}/{version}/signature.json
packages/{name}/{version}/data.sqlite.zst  the download artifact
```

Two properties are enforced rather than assumed, because both failure modes are silent:

**Published versions are immutable.** A version path that already exists is a hard error. Object
storage and CDNs are content-addressed by URL, and a reader who fetched
`packages/terrakit-data-tr/2026.10.1/data.sqlite.zst` an hour ago may still be holding that copy.
Overwriting the path means two clients with the same package name and version hold different bytes,
and no checksum catches it because each verified against the manifest they were each given. The
fix for a bad release is a new version, never a rewritten one.

**URLs are relative to the registry root.** The tree must be position-independent: a mirror, a
CDN failover and an air-gapped copy all have to work by pointing `--registry` at a different
prefix. Absolute URLs pin every reader to wherever the index was first published, so "point at
your mirror" would silently keep reading the origin. `baseUrl` is still recorded at the top of
`index.json`, for consumers that need an absolute URL to hand to something that is not the
client — but the client resolves against its own `--registry`.

**The index is reproducible.** `generatedAt` comes from the newest manifest in the set, not from
the clock, so rebuilding the registry from the same packages yields byte-identical output. That is
what lets `scripts/check_reproducible.sh` cover the registry as well as the packages.

Usage
-----
    tools/build_registry.py datasets/ --out registry/ --base-url https://registry.terrakit.dev
    tools/build_registry.py datasets/ --out registry/ --upload aws:s3://bucket/prefix --production
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
from typing import Any

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

from terrakit_pipeline import release  # noqa: E402

#: Key id of the committed development key. Packages signed with it are refused by --production.
DEV_KEY_ID = release.DEV_KEY_ID

#: Suffix appended to a dataset version to make it a path segment.
#:
#: The dataset version is `2026.10.1+9920` — CalVer plus a content hash of the upstream extract.
#: `+` is legal in a URL path but reads badly in a shell and confuses some S3 clients and CDNs, so
#: the registry uses `-`. The mapping is applied consistently on write and on read, so the CLI can
#: reverse it. Recorded here rather than duplicated in the CLI because a mismatch would make every
#: install 404, and the CLI's error message would say nothing about why.
VERSION_PATH_SEPARATOR = "-"


def relative_path(package: str, version_path: str, filename: str = "manifest.json") -> str:
    """
    A registry path relative to the registry root.

    Every URL a client follows is either relative or resolved against the `--registry` the client
    was given. Absolute URLs would pin the tree to the location it was first published from,
    which makes a mirror, a CDN failover and an offline copy all silently read from the origin
    instead.
    """
    return f"packages/{package}/{version_path}/{filename}"


def version_to_path(version: str) -> str:
    return version.replace("+", VERSION_PATH_SEPARATOR)


def path_to_version(segment: str) -> str:
    return segment.replace(VERSION_PATH_SEPARATOR, "+")


class RegistryError(RuntimeError):
    """A condition that must stop publication."""


def collect(packages_dir: pathlib.Path, *, require_signature: bool) -> list[dict[str, Any]]:
    """Verify and describe every published package under `packages_dir`."""
    if not packages_dir.is_dir():
        raise RegistryError(f"{packages_dir} is not a directory")

    entries: list[dict[str, Any]] = []
    for manifest_path in sorted(packages_dir.glob("terrakit-data-*/manifest.json")):
        package_dir = manifest_path.parent
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

        ok, problems = release.verify_release(package_dir)
        if not ok:
            # Not a warning. Publishing a package that fails its own verification would mean the
            # registry serves bytes the installer rejects, and the failure surfaces to a user as
            # an install error with no way to tell it is upstream's fault.
            raise RegistryError(
                f"{package_dir.name} failed verification:\n"
                + "\n".join(f"    - {p}" for p in problems)
            )

        signature_path = package_dir / "signature.json"
        signature = (
            json.loads(signature_path.read_text(encoding="utf-8"))
            if signature_path.is_file()
            else None
        )

        if require_signature and signature is None:
            raise RegistryError(
                f"{package_dir.name} is unsigned; --production requires a signature"
            )

        database_entry = next(
            (f for f in manifest["files"] if f["path"] == "data.sqlite"), None
        )
        if database_entry is None:
            raise RegistryError(f"{package_dir.name}: manifest has no data.sqlite entry")

        version = manifest["version"]
        artifact_name = release.compressed_filename(
            "data.sqlite", database_entry.get("compression")
        )
        artifact_path = package_dir / artifact_name if artifact_name else package_dir / "data.sqlite"

        entries.append(
            {
                "name": manifest["name"],
                "version": version,
                "manifest": manifest,
                "signature": signature,
                "package_dir": package_dir,
                "artifact_name": artifact_name or "data.sqlite",
                "artifact_path": artifact_path,
                "key_id": signature["keyId"] if signature else None,
            }
        )

    if not entries:
        raise RegistryError(f"no terrakit-data-* packages found under {packages_dir}")
    return entries


def build_index(entries: list[dict[str, Any]], base_url: str) -> dict[str, Any]:
    """`index.json` — every package at its latest version."""
    latest: dict[str, dict[str, Any]] = {}
    for entry in entries:
        current = latest.get(entry["name"])
        # Compare on the version, not on file order: two builds of the same version cannot both
        # be published (§ immutability), so this is a max, not a sort-and-take-last.
        if current is None or _version_key(entry["version"]) > _version_key(current["version"]):
            latest[entry["name"]] = entry

    packages = []
    for name in sorted(latest):
        entry = latest[name]
        manifest = entry["manifest"]
        version_path = version_to_path(entry["version"])
        packages.append(
            {
                "name": name,
                "version": entry["version"],
                "country": manifest.get("country"),
                "level": manifest.get("level"),
                "schemaVersion": manifest.get("schemaVersion"),
                "size": manifest.get("size", {}),
                "entities": manifest.get("entities", {}),
                "license": manifest.get("license"),
                "attribution": manifest.get("attribution", []),
                "generatedAt": manifest.get("generatedAt"),
                "signed": entry["signature"] is not None,
                "keyId": entry["key_id"],
                # Relative, not absolute. An absolute URL makes the tree
                # position-dependent: copying it to a mirror or an air-gapped host and
                # pointing --registry at the copy would still fetch the original, so
                # "point at your mirror" silently does nothing. See relative_path().
                "manifestPath": relative_path(name, version_path),
                "manifestUrl": f"{base_url}/{relative_path(name, version_path)}",
            }
        )

    # Derived from the manifests rather than from the clock. Two runs over the same packages
    # produce the same bytes, so a rebuild does not invalidate every CDN cache entry for no
    # reason — and a cache invalidated for no reason is a registry nobody can operate.
    generated_at = max(
        (p["generatedAt"] or "1970-01-01T00:00:00Z" for p in packages),
        default="1970-01-01T00:00:00Z",
    )

    return {
        "schemaVersion": 1,
        "generatedAt": generated_at,
        "baseUrl": base_url,
        "packageCount": len(packages),
        "packages": packages,
    }


def build_package_index(entry_name: str, entries: list[dict[str, Any]], base_url: str) -> dict[str, Any]:
    """`packages/{name}.json` — every version of one package, with download URLs."""
    versions = []
    for entry in sorted(entries, key=lambda e: _version_key(e["version"]), reverse=True):
        manifest = entry["manifest"]
        version_path = version_to_path(entry["version"])
        artifact = entry["artifact_name"]
        versions.append(
            {
                "version": entry["version"],
                "level": manifest.get("level"),
                "schemaVersion": manifest.get("schemaVersion"),
                "size": manifest.get("size", {}),
                "entities": manifest.get("entities", {}),
                "generatedAt": manifest.get("generatedAt"),
                "signed": entry["signature"] is not None,
                "keyId": entry["key_id"],
                "manifestPath": relative_path(entry_name, version_path, "manifest.json"),
                "downloadUrl": f"{base_url}/{relative_path(entry_name, version_path, artifact)}",
                "downloadPath": relative_path(entry_name, version_path, artifact),
                # Published alongside the artifact so `terrakit add` can reject a corrupt or
                # truncated download BEFORE decompressing it. Verifying only after extraction
                # means a truncated body decompresses to a short database and fails with a
                # SQLite error that says nothing about the network.
                "sha256": _artifact_digest(entry),
                "compressedSha256": _compressed_digest(entry),
            }
        )

    latest = versions[0] if versions else None
    return {
        "schemaVersion": 1,
        "name": entry_name,
        "country": latest and _country_of(entries),
        "latest": latest,
        "versions": versions,
    }


def _version_key(version: str) -> tuple:
    """
    Sortable form of a dataset version.

    `2026.10.1+9920` splits into numbers where possible. Naive string sorting puts `2026.10.10`
    before `2026.10.2`, which would make the registry advertise the older release as latest.
    """
    main, _, suffix = version.partition("+")
    parts: list[int] = []
    for segment in main.split("."):
        parts.append(int(segment) if segment.isdigit() else 0)
    return (parts, int(suffix) if suffix.isdigit() else 0)


def _country_of(entries: list[dict[str, Any]]) -> str | None:
    return entries[0]["manifest"].get("country")


def _artifact_digest(entry: dict[str, Any]) -> str:
    for file_entry in entry["manifest"]["files"]:
        if file_entry["path"] == "data.sqlite":
            return file_entry["sha256"]
    raise RegistryError(f"{entry['name']}: manifest has no data.sqlite digest")


def _compressed_digest(entry: dict[str, Any]) -> str | None:
    for file_entry in entry["manifest"]["files"]:
        if file_entry["path"] == "data.sqlite":
            return file_entry.get("compressedSha256")
    return None


#: CDN cache policy per file class.
#:
#: Version paths are immutable by the rule enforced above, so they may be cached essentially
#: forever; the index must not be, or a published package is invisible until the TTL lapses.
#: Getting this backwards in either direction costs real money or real freshness.
CACHE_POLICY: list[tuple[str, str]] = [
    ("index.json", "public, max-age=300, must-revalidate"),
    ("packages/*.json", "public, max-age=3600, must-revalidate"),
    (
        "packages/*/*/*",
        "public, max-age=31536000, immutable",
    ),
]


def published_digests(out_dir: pathlib.Path) -> dict[tuple[str, str], str]:
    """
    Digest of every artifact currently in a published registry tree.

    Keyed by `(package name, version segment)`. Read from the tree that is actually live —
    not from the staging tree being built — because immutability is a claim about what readers
    and CDNs already hold. An earlier version checked `target.exists()` inside `materialize`,
    which always saw a freshly created staging directory and therefore never fired on a rebuild.
    The one case that matters, rewriting a published version, was the case it silently allowed.
    """
    found: dict[tuple[str, str], str] = {}
    packages_dir = out_dir / "packages"
    if not packages_dir.is_dir():
        return found
    for artifact in packages_dir.glob("*/*/*.zst"):
        version_dir = artifact.parent
        digest = release._sha256(artifact)
        found[(version_dir.parent.name, version_dir.name)] = digest
    return found


def materialize(
    entries: list[dict[str, Any]],
    out_dir: pathlib.Path,
    base_url: str,
    *,
    live: dict[tuple[str, str], str] | None = None,
) -> dict[str, Any]:
    """
    Write the registry tree to `out_dir`.

    `live` is the digest map of the currently published registry. A version present in both with
    different bytes is refused.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    packages_dir = out_dir / "packages"
    packages_dir.mkdir(parents=True, exist_ok=True)
    live = live or {}
    unchanged: list[str] = []

    index = build_index(entries, base_url)
    _write_json(out_dir / "index.json", index)

    by_name: dict[str, list[dict[str, Any]]] = {}
    for entry in entries:
        by_name.setdefault(entry["name"], []).append(entry)

    for name, group in sorted(by_name.items()):
        _write_json(packages_dir / f"{name}.json", build_package_index(name, group, base_url))

        for entry in group:
            version_path = version_to_path(entry["version"])
            target = packages_dir / name / version_path
            artifact = target / entry["artifact_name"]
            incoming = release._sha256(entry["artifact_path"])

            # Immutability gate. Different bytes at an already-published version path is a
            # hard error: a CDN or a reader may still hold the old artifact, each side
            # verifies against the manifest it was handed, and both would report success
            # while holding different data.
            published = live.get((name, version_path))
            if published is not None and published != incoming:
                raise RegistryError(
                    f"refusing to overwrite published version {name}@{entry['version']}.\n"
                    f"    live artifact digest  {published[:12]}\n"
                    f"    incoming digest        {incoming[:12]}\n"
                    f"    Published versions are immutable. Publish a new version instead; "
                    f"if the old release is genuinely broken, publish a fixed version and "
                    f"withdraw the bad one explicitly."
                )

            # Always copy, even when the bytes are unchanged.
            #
            # Skipping an unchanged copy was an optimization that only held when building in
            # place. With a staging tree — which is what makes the swap atomic — `continue`
            # left the artifact out of staging entirely, and the swap then replaced a complete
            # registry with one holding no data files at all. Identical bytes now get copied
            # and the redundancy is reported rather than assumed away.
            if published == incoming:
                unchanged.append(f"{name}@{entry['version']}")

            target.mkdir(parents=True, exist_ok=True)
            shutil.copy2(entry["artifact_path"], artifact)
            shutil.copy2(entry["package_dir"] / "manifest.json", target / "manifest.json")
            signature_path = entry["package_dir"] / "signature.json"
            if signature_path.is_file():
                shutil.copy2(signature_path, target / "signature.json")

    _write_json(out_dir / "cache-policy.json", _cache_policy_document())
    index["_unchanged"] = unchanged
    return index


def swap_into_place(staging: pathlib.Path, out_dir: pathlib.Path) -> None:
    """
    Replace `out_dir` with `staging` in two renames.

    A reader mid-request during a rebuild would otherwise see either a half-written tree or a
    missing one. Renaming the live tree aside and moving the new one in shrinks that window to the
    two syscalls, and it is short enough that a CDN with the cache policy in `cache-policy.json`
    never observes it. `rmtree` followed by `move` would leave a full-directory gap instead.
    """
    if not out_dir.exists():
        out_dir.parent.mkdir(parents=True, exist_ok=True)
        os.rename(staging, out_dir)
        return

    retired = out_dir.with_name(out_dir.name + ".retired")
    if retired.exists():
        shutil.rmtree(retired)
    os.rename(out_dir, retired)
    try:
        os.rename(staging, out_dir)
    except OSError:
        # Put the old tree back rather than leaving nothing at all.
        os.rename(retired, out_dir)
        raise
    shutil.rmtree(retired, ignore_errors=True)


def _cache_policy_document() -> dict[str, Any]:
    return {
        "note": (
            "Cache-Control per file class. Version paths are immutable and may be cached "
            "indefinitely; the index must revalidate or a new release stays invisible."
        ),
        "rules": [{"pattern": pattern, "cacheControl": value} for pattern, value in CACHE_POLICY],
    }


def _write_json(path: pathlib.Path, document: dict[str, Any]) -> None:
    # Sorted keys and a fixed indent so the output is byte-stable across runs, which is what
    # makes the registry reproducible and keeps CDN revalidation a no-op when nothing changed.
    path.write_text(
        json.dumps(document, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def upload(out_dir: pathlib.Path, target: str, *, dry_run: bool) -> int:
    """
    Sync the registry tree to object storage.

    Supports `aws:s3://bucket/prefix` and `rclone:remote:path`. Both are shell-outs rather than
    SDK dependencies: a publisher runs this on a machine that already has the tooling, and a
    Python AWS SDK would add 20 MB of dependencies to a script whose whole job is to copy files.
    """
    if ":" not in target:
        raise RegistryError(
            f"--upload target {target!r} has no 'tool:destination' form; "
            f"use aws:s3://bucket/prefix or rclone:remote:path"
        )
    tool, _, destination = target.partition(":")

    if dry_run:
        total = sum(f.stat().st_size for f in out_dir.rglob("*") if f.is_file())
        print(f"  would {tool} sync {out_dir} -> {destination} ({total:,} bytes)")
        return 0

    if tool == "aws":
        if not shutil.which("aws"):
            raise RegistryError("the `aws` CLI is not on PATH")
        command = ["aws", "s3", "sync", str(out_dir), destination, "--no-progress"]
    elif tool == "rclone":
        if not shutil.which("rclone"):
            raise RegistryError("the `rclone` CLI is not on PATH")
        command = ["rclone", "sync", str(out_dir), destination]
    else:
        raise RegistryError(f"unknown upload tool {tool!r}; use aws or rclone")

    result = subprocess.run(command, check=False)
    return result.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("packages", type=pathlib.Path, help="directory of published packages")
    parser.add_argument("--out", type=pathlib.Path, required=True, help="registry tree to write")
    parser.add_argument(
        "--base-url",
        default="https://registry.terrakit.dev",
        help="public base URL written into the index. Must match where the tree is served, "
             "because the CLI downloads from these absolute URLs.",
    )
    parser.add_argument(
        "--upload",
        default=None,
        metavar="TOOL:DEST",
        help="sync after building, e.g. aws:s3://my-bucket/terrakit",
    )
    parser.add_argument(
        "--production",
        action="store_true",
        help="refuse packages signed with the committed development key, and require a signature",
    )
    parser.add_argument("--dry-run", action="store_true", help="verify only; write nothing")

    args = parser.parse_args(argv)

    try:
        entries = collect(args.packages, require_signature=args.production)
    except RegistryError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 3

    if args.production:
        dev_signed = [e for e in entries if e["key_id"] == DEV_KEY_ID]
        if dev_signed:
            names = ", ".join(sorted(e["name"] for e in dev_signed))
            print(
                f"error: refusing to publish development-signed packages: {names}\n"
                f"    The development key is committed to the repository, so its private half is "
                f"public. A package signed with it would look verified to anyone who read this "
                f"repository.\n"
                f"    Sign with the release key: terrakit-pipeline publish <dir> "
                f"--sign-key <release-key> --sign-key-id <release-id>",
                file=sys.stderr,
            )
            return 3
        unsigned = [e for e in entries if e["signature"] is None]
        if unsigned:
            names = ", ".join(sorted(e["name"] for e in unsigned))
            print(
                f"error: refusing to publish unsigned packages: {names}\n"
                f"    --production requires every package to be signed.",
                file=sys.stderr,
            )
            return 3

    print(f"registry: {len(entries)} package version(s) from {args.packages}")

    if args.dry_run:
        for entry in sorted(entries, key=lambda e: (e["name"], _version_key(e["version"]))):
            size = entry["manifest"].get("size", {})
            print(
                f"  {entry['name']}@{entry['version']} "
                f"[{entry['key_id'] or 'UNSIGNED'}] "
                f"raw={size.get('raw', 0):,} compressed={size.get('compressed') or '-'}"
            )
        print("dry run: nothing written")
        return 0

    out_dir = args.out.resolve()

    # Read what is live BEFORE touching anything. This is the immutability reference: the
    # rebuild is exactly the case where a published version could be silently replaced.
    live = published_digests(out_dir)
    if live:
        print(f"  live registry: {len(live)} published version(s), immutable")

    # Stage a complete tree, then swap it in. A failure partway through leaves the live tree
    # untouched rather than half-updated — and a half-updated registry is one a CDN will serve,
    # because a CDN does not know it is half-updated.
    staging_root = out_dir.parent
    staging_root.mkdir(parents=True, exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(dir=staging_root) as tmp:
            staging = pathlib.Path(tmp) / "registry"
            index = materialize(entries, staging, args.base_url, live=live)
            swap_into_place(staging, out_dir)
    except RegistryError as exc:
        # A traceback here would bury the one line that matters — which version, which
        # digests, and what to do instead — under a stack through the staging code.
        print(f"error: {exc}", file=sys.stderr)
        print("       Nothing was published; the live registry is unchanged.", file=sys.stderr)
        return 3

    unchanged = index.pop("_unchanged", [])
    if unchanged:
        print(f"  unchanged at an existing published version: {', '.join(sorted(unchanged))}")
    print(f"  index.json: {index['packageCount']} package(s), generatedAt={index['generatedAt']}")
    for package in index["packages"]:
        print(f"    {package['name']}@{package['version']} ({package['level']}, {package['country']})")

    if args.upload:
        try:
            code = upload(args.out, args.upload, dry_run=False)
        except RegistryError as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 2
        if code != 0:
            print(f"error: upload failed with exit code {code}", file=sys.stderr)
            return code
        print(f"  uploaded to {args.upload}")

    return 0


if __name__ == "__main__":
    sys.exit(main())