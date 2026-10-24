import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

SCRIPT = Path(__file__).with_name('wine-bootstrap-addons.py')
spec = importlib.util.spec_from_file_location('addons', SCRIPT)
addons = importlib.util.module_from_spec(spec)
spec.loader.exec_module(addons)

SOURCE = '''#define GECKO_VERSION "2.47.4"
#define GECKO_ARCH "x86"
#define GECKO_SHA "{x86}"
#define GECKO_ARCH "x86_64"
#define GECKO_SHA "{x64}"
#define MONO_VERSION "11.3.0"
#define MONO_ARCH "x86"
#define MONO_SHA "{mono}"
#define MONO_ARCH "arm64"
#define MONO_SHA "{arm}"
'''.format(x86='a' * 64, x64='b' * 64, mono='c' * 64, arm='d' * 64)


class Response(io.BytesIO):
    def geturl(self):
        return 'https://example.invalid/installer.msi'


class AddonTests(unittest.TestCase):
    def test_wine_defines_both_gecko_architectures_and_the_x86_mono_checksum(self):
        entries = addons.prerequisites(SOURCE)
        self.assertEqual([entry[0] for entry in entries], [
            'wine-gecko-2.47.4-x86.msi', 'wine-gecko-2.47.4-x86_64.msi', 'wine-mono-11.3.0-x86.msi'])
        self.assertEqual([entry[2] for entry in entries], ['a' * 64, 'b' * 64, 'c' * 64])
        self.assertTrue(all(entry[1].startswith('https://dl.winehq.org/') for entry in entries))

    def test_ambiguous_or_unknown_source_layout_fails_closed(self):
        for source in (SOURCE.replace('GECKO_VERSION', 'OTHER_VERSION'),
                       SOURCE + '#define MONO_VERSION "11.3.0"\n',
                       SOURCE.replace('#define MONO_ARCH "x86"', '#define MONO_ARCH "unknown"'),
                       SOURCE.replace('a' * 64, 'invalid'),
                       SOURCE + '#define GECKO_ARCH "x86"\n#define GECKO_SHA "' + 'e' * 64 + '"\n'):
            with self.assertRaises(ValueError):
                addons.prerequisites(source)

    def test_verified_download_is_atomic_and_a_good_cache_needs_no_network(self):
        data = b'fixture installer'
        checksum = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / 'cache'
            with patch.object(addons.urllib.request, 'urlopen', return_value=Response(data)):
                addons.cached_installer(cache, 'fixture.msi', 'https://example.invalid/fixture', checksum)
            self.assertEqual((cache / 'fixture.msi').read_bytes(), data)
            with patch.object(addons.urllib.request, 'urlopen') as network:
                addons.cached_installer(cache, 'fixture.msi', 'https://example.invalid/fixture', checksum)
                network.assert_not_called()
            self.assertEqual(len(list(cache.iterdir())), 1)

    def test_bad_or_oversized_download_never_enters_the_cache(self):
        for bound, checksum in ((100, '0' * 64), (1, hashlib.sha256(b'bad').hexdigest())):
            with tempfile.TemporaryDirectory() as temporary:
                cache = Path(temporary) / 'cache'
                with patch.object(addons, 'MAX_INSTALLER_BYTES', bound), \
                     patch.object(addons.urllib.request, 'urlopen', return_value=Response(b'bad')):
                    with self.assertRaises(ValueError):
                        addons.cached_installer(cache, 'fixture.msi', 'https://example.invalid/fixture', checksum)
                self.assertEqual(list(cache.iterdir()), [])

    def test_existing_bad_or_linked_files_are_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            target = cache / 'fixture.msi'
            target.write_bytes(b'existing')
            with self.assertRaises(ValueError):
                addons.cached_installer(cache, target.name, 'https://example.invalid/fixture', '0' * 64)
            self.assertEqual(target.read_bytes(), b'existing')
            target.unlink()
            target.symlink_to(cache / 'other')
            with self.assertRaises(ValueError):
                addons.cached_installer(cache, target.name, 'https://example.invalid/fixture', '0' * 64)

    def test_network_failures_do_not_print_redirect_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            runtimes = Path(temporary) / 'runtimes.json'
            runtimes.write_text(json.dumps([{'id': 'wine-macos', 'arch': 'x86_64', 'version': '11.18'}]))
            output = io.StringIO()
            with patch.dict(os.environ, VGAMES_WINE_E2E_RUNTIMES=str(runtimes)), \
                 patch('urllib.request.urlopen', side_effect=urllib.error.URLError('fixture-private-value')), \
                 contextlib.redirect_stderr(output), self.assertRaises(SystemExit) as stopped:
                runpy.run_path(str(SCRIPT), run_name='__main__')
            self.assertEqual(stopped.exception.code, 1)
            self.assertNotIn('fixture-private-value', output.getvalue())
