import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import mtk_dependencies as runtime


class RuntimeDeliveryTests(unittest.TestCase):
    def test_pins_cover_four_platforms_and_no_optional_gui_or_payload_packages(self):
        pins = json.loads(runtime.PINS.read_text())
        self.assertEqual(set(pins['platforms']), {'linux-x86_64', 'macos-aarch64', 'macos-x86_64', 'windows-x86_64'})
        for pin in pins['platforms'].values():
            self.assertEqual({p['package'] for p in pin['wheels']}, {'pyusb', 'pyserial', 'colorama', 'pycryptodome', 'pycryptodomex', 'libusb-package', 'importlib-resources'})
            for item in [pin['python'], *pin['wheels']]:
                self.assertEqual(len(item['sha256']), 64)
                self.assertTrue(item['url'].startswith('https://'))
        inventory = json.loads(runtime.INVENTORY.read_text())
        self.assertEqual(runtime.sha(runtime.INVENTORY), pins['mtk_source']['inventory_sha256'])
        self.assertFalse(inventory['loader_included'])
        for name in inventory['files']:
            self.assertTrue(name.endswith('.py') or name in ('LICENSE', 'README.md', 'requirements.txt', 'pyproject.toml'))

    def test_download_collision_and_corrupt_cache_fail_before_execution_or_network(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); pin = {'filename': 'source.tar.gz', 'url': 'https://example.invalid/source', 'size': 7, 'sha256': hashlib.sha256(b'fixture').hexdigest()}
            pending = root / 'source.tar.gz.download'; pending.write_bytes(b'other run')
            with patch.object(runtime, 'urlopen') as network:
                with self.assertRaises(FileExistsError): runtime.fetch(root, pin)
                network.assert_not_called()
            self.assertEqual(pending.read_bytes(), b'other run')
            (root / pin['filename']).write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError, 'differs'): runtime.fetch(root, pin, offline=True)

    def test_cache_swap_after_fetch_is_rejected_before_any_extraction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); cache = root / 'cached'; cache.write_bytes(b'original')
            pin = {'filename': 'archive', 'size': 8, 'sha256': hashlib.sha256(b'original').hexdigest()}
            pins = {'platforms': {'fixture': {'python': pin, 'wheels': [], 'site_packages': 'site'}},
                    'mtk_source': {**pin, 'inventory_sha256': runtime.sha(runtime.INVENTORY)}}
            pinfile = root / 'pins.json'; pinfile.write_text(json.dumps(pins))
            def swapped_fetch(*args):
                cache.write_bytes(b'modified')
                return cache
            with patch.object(runtime, 'PINS', pinfile), patch.object(runtime, 'fetch', side_effect=swapped_fetch), patch.object(runtime, 'unpack_python') as extract:
                with self.assertRaisesRegex(ValueError, 'changed before extraction'):
                    runtime.prepare('fixture', root / 'cache', root / 'output')
                extract.assert_not_called()
            self.assertFalse((root / 'output/runtime.json').exists())
            self.assertFalse((root / 'output').exists())

    def test_output_inside_git_checkout_is_refused_before_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); (root / '.git').write_text('gitdir: fixture')
            with patch.object(runtime, 'fetch') as fetch:
                with self.assertRaisesRegex(ValueError, 'outside Git'):
                    runtime.prepare('linux-x86_64', root / 'cache', root / 'output')
                fetch.assert_not_called()

    def test_download_uses_bounded_reads_and_total_deadline(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); pin = {'filename': 'source', 'url': 'https://example.invalid/source', 'size': 7, 'sha256': hashlib.sha256(b'fixture').hexdigest()}
            class Response:
                url = pin['url']
                def __enter__(self): return self
                def __exit__(self, *args): pass
                def read(self, *args): raise AssertionError('Unbounded read must not run')
                def read1(self, size): return b'fixture'
            with patch.object(runtime, 'urlopen', return_value=Response()), patch.object(runtime.time, 'monotonic', side_effect=[0, 0, 181]):
                with self.assertRaisesRegex(ValueError, 'deadline exceeded'): runtime.fetch(root, pin)
            self.assertFalse((root / 'source').exists())
            self.assertFalse((root / 'source.download').exists())

    def test_python_aliases_flatten_safely_and_escape_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            def create(target):
                path = root / 'python.tar.gz'
                with tarfile.open(path, 'w:gz') as archive:
                    member = tarfile.TarInfo('python/bin/python3.12'); member.size = 7; member.mode = 0o755
                    archive.addfile(member, io.BytesIO(b'fixture'))
                    alias = tarfile.TarInfo('python/bin/python3'); alias.type = tarfile.SYMTYPE; alias.linkname = target
                    archive.addfile(alias)
                return path
            runtime.unpack_python(create('python3.12').read_bytes(), root / 'safe')
            alias = root / 'safe/python/bin/python3'
            self.assertFalse(alias.is_symlink()); self.assertEqual(alias.read_bytes(), b'fixture')
            for target in ('../../../outside', '/outside', 'python3'):
                with self.assertRaises(ValueError): runtime.unpack_python(create(target).read_bytes(), root / target.replace('/', '_'))

    def test_wheel_traversal_and_unreviewed_install_hooks_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ('../escape', 'drive:/escape', 'module.data/scripts/run'):
                path = root / 'wheel.whl'
                with zipfile.ZipFile(path, 'w') as wheel: wheel.writestr(name, b'fixture')
                with self.assertRaises(ValueError): runtime.unpack_wheel(path.read_bytes(), root / 'out', 'site')
            self.assertFalse((root / 'escape').exists())
            raw = io.BytesIO()
            with zipfile.ZipFile(raw, 'w') as wheel: wheel.writestr('pkgXfile', b'fixture')
            # ZipInfo.filename truncates NUL while orig_filename retains it.
            with self.assertRaisesRegex(ValueError, 'Unsafe runtime archive path'):
                runtime.unpack_wheel(raw.getvalue().replace(b'pkgXfile', b'pkg\x00file'), root / 'nul', 'site')

    def test_mtk_extraction_selects_only_exact_reviewed_sources_not_bundled_da(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); path = root / 'mtk.tar.gz'; data = b'# fixture\n'; revision = 'a' * 40
            inventory = {'revision': revision, 'files': {'mtkclient/__init__.py': {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}}}
            with tarfile.open(path, 'w:gz') as archive:
                for name, value in [('mtkclient/__init__.py', data), ('mtkclient/Loader/private.bin', b'never ship')]:
                    member = tarfile.TarInfo('mtkclient-' + revision + '/' + name); member.size = len(value); archive.addfile(member, io.BytesIO(value))
            runtime.unpack_mtk(path.read_bytes(), root / 'out', inventory)
            self.assertEqual(list(runtime.file_inventory(root / 'out')), ['mtk/mtkclient/__init__.py'])
            inventory['files']['mtkclient/__init__.py']['sha256'] = '0' * 64
            with self.assertRaisesRegex(ValueError, 'hash differs'): runtime.unpack_mtk(path.read_bytes(), root / 'bad', inventory)

    def test_untrusted_receipt_or_modified_executable_cannot_be_smoked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); runtime.write_file(root, 'python', b'fixture'); runtime.write_file(root, 'libusb', b'fixture')
            receipt = {'schema': 1, 'kind': 'couch-owner-mtk-runtime', 'complete': True, 'loader_included': False,
                       'prebuilt_mtk_payloads_included': False, 'pins_sha256': runtime.sha(runtime.PINS),
                       'files': runtime.file_inventory(root), 'executables': {'python': 'python'}, 'native_libraries': {'libusb': 'libusb'},
                       'mtk': {'source_inventory_sha256': runtime.sha(runtime.INVENTORY)}}
            (root / 'runtime.json').write_text(json.dumps(receipt)); checksum = runtime.sha(root / 'runtime.json')
            runtime.verify(root, checksum)
            with patch.object(runtime.subprocess, 'run') as execute:
                with self.assertRaisesRegex(ValueError, 'trusted digest'): runtime.smoke(root, '0' * 64)
                (root / 'python').write_bytes(b'changed')
                with self.assertRaisesRegex(ValueError, 'trusted receipt'): runtime.smoke(root, checksum)
                execute.assert_not_called()


if __name__ == '__main__':
    unittest.main()
