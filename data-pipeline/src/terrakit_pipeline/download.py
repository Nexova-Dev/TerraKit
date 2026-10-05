"""
Source downloader with caching and checksum recording.

Implements the 'Downloader' stage of SPEC section 27.

Design notes that matter:

- Cache is keyed by filename under a single directory, so re-running the
  pipeline does not re-download gigabytes. The recorded checksum is what makes
  the cache safe to reuse: a changed upstream file is detected, not silently
  consumed.
- `retrieved_at` and `checksum` are recorded per source because LICENSING.md
  section 6 fails the build without them.
- No progress or retry behaviour beyond a bounded retry count. A flaky network
  should fail the build loudly rather than produce a package from partial data.
"""

from __future__ import annotations

import hashlib
import json
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from .geonames import SOURCES, SourceSpec

__all__ = ["Downloader", "DownloadedSource", "fetch_all"]

USER_AGENT = "terrakit-pipeline/0.1 (+https://github.com/NexovaDev/terrakit)"


@dataclass
class DownloadedSource:
    spec: SourceSpec
    path: Path
    sha256: str
    size: int
    retrieved_at: str
    from_cache: bool
    # False when the upstream dataset genuinely does not exist. Optional
    # sources only; a required source that is unavailable raises instead.
    available: bool = True
    unavailable_reason: str = ""


class Downloader:
    def __init__(self, cache_dir: Path, *, timeout: int = 120, retries: int = 3):
        self.cache_dir = cache_dir
        self.timeout = timeout
        self.retries = retries
        self.cache_dir.mkdir(parents=True, exist_ok=True)

    def fetch(
        self,
        spec: SourceSpec,
        *,
        iso2: str | None = None,
        optional: bool = False,
    ) -> DownloadedSource:
        """
        Fetch one source.

        `optional=True` means "this dataset may legitimately not exist". Georgia
        has no GeoNames postal file (404), and Georgia is an MVP country per
        spec section 48, so treating that as a hard failure would block a
        country we must ship. A missing optional source yields an
        `available=False` descriptor; a missing required source raises.
        """
        url = spec.url.format(iso2=iso2) if iso2 else spec.url
        filename = spec.filename.format(iso2=iso2) if iso2 else spec.filename
        target = self.cache_dir / filename

        if target.exists():
            # A cached file can be a saved error page: an interrupted or
            # pre-fix run may have written a 404 body to GE.zip, and treating
            # that as a valid cache hit fails much later with a confusing
            # BadZipFile from the parser.
            if target.name.endswith(".zip") and not _is_zip(target):
                if optional:
                    target.unlink()
                else:
                    raise RuntimeError(
                        f"cached file {target} is not a valid zip archive; "
                        f"delete it and re-run"
                    )
            digest = self.sha256(target)
            return DownloadedSource(
                spec=spec,
                path=target,
                sha256=digest,
                size=target.stat().st_size,
                retrieved_at=datetime.fromtimestamp(
                    target.stat().st_mtime, tz=UTC
                ).isoformat().replace("+00:00", "Z"),
                from_cache=True,
                available=True,
            )

        last_error: Exception | None = None
        for attempt in range(1, self.retries + 1):
            try:
                request = urllib.request.Request(
                    url, headers={"User-Agent": USER_AGENT}
                )
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    data = response.read()
                break
            except urllib.error.HTTPError as exc:
                # 404/403 is a statement about the dataset, not a transient
                # failure. Retrying wastes three round trips to learn nothing.
                if exc.code in (403, 404) and optional:
                    return DownloadedSource(
                        spec=spec,
                        path=target,
                        sha256="",
                        size=0,
                        retrieved_at=datetime.now(tz=UTC).isoformat().replace(
                            "+00:00", "Z"
                        ),
                        from_cache=False,
                        available=False,
                        unavailable_reason=f"HTTP {exc.code} from {url}",
                    )
                last_error = exc
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                last_error = exc
            if attempt == self.retries:
                raise RuntimeError(
                    f"failed to download {url} after {self.retries} attempts: {last_error}"
                )
            # Bounded backoff. Failing the build is preferable to retrying
            # forever against a source that is genuinely gone.
            time.sleep(2 ** attempt)
        else:  # pragma: no cover - loop always breaks or raises
            raise RuntimeError(f"failed to download {url}: {last_error}")

        # Write via a temp file so an interrupted download never leaves a
        # truncated file in the cache that a later run would treat as valid.
        temp = target.with_suffix(target.suffix + ".part")
        temp.write_bytes(data)
        temp.replace(target)

        return DownloadedSource(
            spec=spec,
            path=target,
            sha256=hashlib.sha256(data).hexdigest(),
            size=len(data),
            retrieved_at=datetime.now(tz=UTC).isoformat().replace("+00:00", "Z"),
            from_cache=False,
        )

    @staticmethod
    def sha256(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()


def _is_zip(path: Path) -> bool:
    """Local magic-number check, so a saved error page is not mistaken for data."""
    try:
        with path.open("rb") as handle:
            return handle.read(4) == b"PK\x03\x04"
    except OSError:
        return False


def fetch_all(
    cache_dir: Path,
    country_iso2: str,
    *,
    need_postal: bool = True,
    log=print,
) -> dict[str, DownloadedSource]:
    """Fetch every source a `cities`/`postal` package needs."""
    downloader = Downloader(cache_dir)
    out: dict[str, DownloadedSource] = {}

    for key in ("countryInfo", "admin1", "admin2", "cities500"):
        spec = SOURCES[key]
        log(f"  fetching {spec.filename}")
        out[key] = downloader.fetch(spec)

    if need_postal:
        spec = SOURCES["postal"]
        log(f"  fetching {spec.filename.format(iso2=country_iso2)}")
        # Optional: GeoNames publishes no postal file for some countries
        # (Georgia is one, and it is an MVP country per spec section 48).
        source = downloader.fetch(spec, iso2=country_iso2, optional=True)
        if source.available:
            out["postal"] = source
        else:
            log(f"    not published upstream: {source.unavailable_reason}")

    provenance = cache_dir / "provenance.json"
    if provenance.exists():
        try:
            recorded = json.loads(provenance.read_text(encoding="utf-8"))
            for key, value in recorded.items():
                if key in out and value.get("sha256") != out[key].sha256:
                    log(
                        f"  WARNING: {out[key].spec.filename} changed upstream "
                        f"({value.get('sha256', '?')[:12]} -> {out[key].sha256[:12]})"
                    )
        except json.JSONDecodeError:
            log("  WARNING: provenance.json is unreadable; skipping change check")

    provenance.write_text(
        json.dumps(
            {
                key: {
                    "sha256": item.sha256,
                    "url": item.spec.url,
                    "retrievedAt": item.retrieved_at,
                }
                for key, item in sorted(out.items())
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    return out
