"""Regression checks for archive transport and legacy cache recovery."""
import hashlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'src'))
from terrakit_pipeline.download import Downloader
from terrakit_pipeline.geonames import SOURCES
from terrakit_pipeline.__main__ import _load_cached


def archive(name, content):
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w') as z:
        z.writestr(name, content)
    return data.getvalue()


class DownloadTests(unittest.TestCase):
    def test_fresh_download_and_cache_have_same_payload_digest(self):
        content = 'Istanbul\tİstanbul\n'.encode()
        with tempfile.TemporaryDirectory() as tmp:
            downloader = Downloader(Path(tmp))
            with patch('urllib.request.urlopen', return_value=io.BytesIO(archive('cities500.txt', content))):
                result = downloader.fetch(SOURCES['cities500'])
            self.assertEqual(result.path.read_bytes(), content)
            self.assertEqual(result.sha256, hashlib.sha256(content).hexdigest())
            self.assertEqual(result.size, len(content))
            with patch('urllib.request.urlopen', side_effect=AssertionError('network')):
                cached = downloader.fetch(SOURCES['cities500'])
            self.assertEqual(cached.sha256, result.sha256)

    def test_postal_stays_zip_and_is_available_offline(self):
        data = archive('TR.txt', b'postal rows')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch('urllib.request.urlopen', return_value=io.BytesIO(data)):
                result = Downloader(root).fetch(SOURCES['postal'], iso2='TR')
            self.assertEqual(result.path.name, 'TR.zip')
            cached = _load_cached(root, 'TR', need_postal=True, need_alternate_names=False)
            self.assertEqual(cached['postal'].path.read_bytes(), data)

    def test_legacy_archives_recovered_offline(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'cities500.txt').write_bytes(archive('cities500.txt', b'cities'))
            (root / 'TR.txt').write_bytes(archive('TR.txt', b'postal'))
            cached = _load_cached(root, 'TR', need_postal=True, need_alternate_names=False)
            self.assertEqual(cached['cities500'].path.read_bytes(), b'cities')
            self.assertTrue(zipfile.is_zipfile(cached['postal'].path))

    def test_invalid_optional_cache_is_refetched(self):
        data = archive('TR.txt', b'postal rows')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'TR.zip').write_text('upstream error page')
            with patch('urllib.request.urlopen', return_value=io.BytesIO(data)):
                result = Downloader(root).fetch(SOURCES['postal'], iso2='TR', optional=True)
            self.assertEqual(result.path.read_bytes(), data)
            self.assertFalse(result.from_cache)

    def test_missing_member_does_not_publish_cache(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch('urllib.request.urlopen', return_value=io.BytesIO(archive('../unexpected', b'bad'))):
                with self.assertRaises(KeyError):
                    Downloader(root).fetch(SOURCES['cities500'])
            self.assertFalse((root / 'cities500.txt').exists())
            self.assertFalse((root / 'cities500.txt.part.unpacked').exists())


if __name__ == '__main__':
    unittest.main()
