"""
Version and identity helpers.

Two independent version numbers, per SPEC section 31:

- GENERATOR_VERSION: this pipeline's software version.
- dataset_version_for(): a CalVer dataset version derived from input content.

Deriving the dataset version from the source checksum rather than the wall clock
is what makes a rebuild reproducible. Two builds from identical inputs get an
identical version AND identical bytes; a version derived from a date would claim
a new version for unchanged data.
"""

from __future__ import annotations

import hashlib

__all__ = ["GENERATOR_VERSION", "dataset_version_for", "short_hash"]

GENERATOR_VERSION = "0.1.0"


def short_hash(sha256_hex: str, length: int = 12) -> str:
    return sha256_hex[:length]


def dataset_version_for(sources_sha256: str) -> str:
    """
    CalVer derived from input content: YYYY.MM.N, per SPEC section 31.

    The date part comes from SOURCE_DATE_EPOCH so it is reproducible, and N is a
    monotonic counter bumped by the maintainer when a rebuild for the same date
    legitimately differs (an upstream source was corrected, for instance).

    With no epoch set we fall back to a fixed constant rather than
    time.time(), because an unset variable must not silently make every build
    produce different output.
    """
    import os
    from datetime import UTC, datetime

    # Imported rather than redeclared: a second copy of the fallback epoch is a
    # second thing to forget to update.
    from .package_builder import _DEFAULT_SOURCE_DATE_EPOCH

    raw_epoch = os.environ.get("SOURCE_DATE_EPOCH")
    epoch = int(raw_epoch) if raw_epoch else _DEFAULT_SOURCE_DATE_EPOCH
    when = datetime.fromtimestamp(epoch, tz=UTC)

    counter = os.environ.get("TERRAKIT_DATASET_COUNTER", "1")
    digest = hashlib.sha256(sources_sha256.encode("utf-8")).hexdigest()[:4]
    return f"{when.year:04d}.{when.month:02d}.{counter}+{digest}"
