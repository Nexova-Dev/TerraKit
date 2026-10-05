"""
TerraKit data pipeline.

Stages, per SPEC section 27:

    Downloader -> Parser -> Normalizer -> Deduplicator -> Hierarchy Builder
    -> Validator -> Database Generator -> SQLite Generator -> Package Builder
    -> Checksum/Signature -> Registry

Implemented here: Normalizer, Deduplicator, Hierarchy Builder, SQLite Generator,
Package Builder, Checksum. Downloader lives in download.py, validation in
validator.py, and Registry publication is a CDN upload, not a code path.

Entry point: python -m terrakit_pipeline build TR
"""

__version__ = "0.1.0"

from .normalize import normalize, postal_normalize, search_key_unaccented

__all__ = ["normalize", "postal_normalize", "search_key_unaccented", "__version__"]
