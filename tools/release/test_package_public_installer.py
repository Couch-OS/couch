import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import package_public_installer as package


class PublicInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fixture_root = self.root / 'source'
        for directory in ('userdata', 'ramdisk', 'boot', 'logo', 'source/kernel',
                          'source/couch-installer/tools/installer/pins', 'source/assets/boot'):
            (self.root / directory).mkdir(parents=True)
        (self.fixture_root / 'assets/boot/couch.png').write_bytes(b'canonical PNG fixture')
        payloads = {name: ('public fixture ' + name).encode() for name in package.FILES}
        payloads['logo.bgra'] = bytes(480 * 800 * 4)
        self.files = {name: {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
                      for name, data in payloads.items()}
        for name, data in payloads.items():
            directory = 'userdata' if name == 'userdata.ext4' else 'ramdisk' if name == 'installer.cpio.gz' else 'logo' if name == 'logo.bgra' else 'boot'
            (self.root / directory / name).write_bytes(data)
        self.save(self.fixture_root / 'kernel/release-pin.json',
                  {'source_commit': 'b'*40, 'zimage_sha256': self.files['zImage']['sha256']})
        self.save(self.fixture_root / 'couch-installer/tools/installer/pins/ha100_official_runtime.json', {'sha256': 'c'*64})
        self.receipts = {
            'userdata': dict(schema=1, kind='couch-owner-neutral-userdata', installable=False,
                             private_only=False, image=dict(path='userdata.ext4', format='raw-ext4', **self.files['userdata.ext4']),
                             geometry={'size': 8 * 1024**3}, owner_vendor_source_sha256='c'*64),
            'ramdisk': dict(schema=1, kind='couch-owner-neutral-ramdisk', bootable=False,
                            file='installer.cpio.gz', owner_vendor_source_sha256='c'*64, **self.files['installer.cpio.gz']),
            'boot': dict(schema=1, kind='couch-owner-neutral-boot-payloads', bootable=False,
                         source_kernel_commit='b'*40, files={name: self.files[name] for name in ('boot.cpio.gz', 'recovery.cpio.gz', 'zImage')}),
            'logo': dict(schema=1, kind='couch-public-logo-frame', file='logo.bgra',
                         source_sha256=hashlib.sha256(b'canonical PNG fixture').hexdigest(), **self.files['logo.bgra'])}
        self.source_bytes = self.git_archive('a'*40)
        (self.root / 'source.tar.gz').write_bytes(self.source_bytes)
        self.attestation = dict(schema=1, kind='couch-public-os-build', source_commit='a'*40,
                                source_archive_sha256=hashlib.sha256(self.source_bytes).hexdigest(), complete=True, private_inputs=False,
                                files=self.files, builder_receipts={})
        self.write_receipts()
        self.patch = patch.object(package, 'ROOT', self.fixture_root)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    @staticmethod
    def git_archive(commit):
        output = io.BytesIO()
        headers = {'comment': commit} if commit is not None else {}
        with tarfile.open(fileobj=output, mode='w:gz', format=tarfile.PAX_FORMAT, pax_headers=headers) as archive:
            item = tarfile.TarInfo('README.md')
            item.size = 8
            archive.addfile(item, io.BytesIO(b'fixture\n'))
        return output.getvalue()

    def save(self, path, value):
        path.write_bytes(package.encoded(value))

    def write_receipts(self):
        for kind, name in [('userdata', 'image.json'), ('ramdisk', 'ramdisk.json'), ('boot', 'boot.json'), ('logo', 'logo.json')]:
            path = self.root / kind / name
            self.save(path, self.receipts[kind])
            self.attestation['builder_receipts'][kind] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        self.save(self.root / 'build.json', self.attestation)

    def build(self, output='output'):
        return package.prepare(self.root / 'build.json', self.root / 'source.tar.gz', *(self.root / name for name in ('userdata', 'ramdisk', 'boot', 'logo')),
                               self.root / output, 'v1.2.3-alpha.1')

    def test_deterministic_exact_public_archive_and_launcher_descriptor(self):
        first = self.build()
        second = self.build('second')
        self.assertEqual(first, second)
        name = first['payload']['url'].rsplit('/', 1)[-1]
        data = (self.root / 'output' / name).read_bytes()
        self.assertEqual(data, (self.root / 'second' / name).read_bytes())
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            members = archive.getmembers()
            self.assertEqual([m.name for m in members], sorted([*package.FILES, 'manifest.json']))
            self.assertTrue(all(m.isreg() and m.mode == 0o644 and m.mtime == 0 and not m.pax_headers for m in members))
            manifest = json.load(archive.extractfile('manifest.json'))
            self.assertEqual(manifest['files'], self.files)
            self.assertEqual(manifest['source_commit'], 'a'*40)
        descriptor = json.loads((self.root / 'output/installer.json').read_bytes())
        self.assertEqual(set(descriptor), {'schema', 'kind', 'model', 'version', 'source_commit', 'payload'})
        self.assertEqual(descriptor['payload']['sha256'], hashlib.sha256(data).hexdigest())
        self.assertFalse(first['published'])
        # Unrelated owner files in input directories are never enumerated/copied.
        (self.root / 'boot/boot.img').write_bytes(b'owner fixture')
        self.assertEqual(self.build('third'), first)

    def test_mutated_files_receipts_and_mixed_inventory_are_refused_before_output(self):
        path = self.root / 'boot/zImage'
        original = path.read_bytes()
        path.write_bytes(b'changed')
        with self.assertRaises(ValueError):
            self.build()
        path.write_bytes(original)
        self.attestation['files']['zImage'] = {'size': len(original), 'sha256': 'e'*64}
        self.save(self.root / 'build.json', self.attestation)
        with self.assertRaises(ValueError):
            self.build()
        self.assertFalse((self.root / 'output').exists())

    def test_private_kinds_extra_names_and_incomplete_attestation_are_refused(self):
        for field, value in [('private_inputs', True), ('complete', False), ('source_commit', 'main')]:
            saved = self.attestation[field]
            self.attestation[field] = value
            self.save(self.root / 'build.json', self.attestation)
            with self.assertRaises(ValueError):
                self.build()
            self.attestation[field] = saved
        self.attestation['files']['boot.img'] = dict(size=7, sha256='f'*64)
        self.write_receipts()
        with self.assertRaises(ValueError):
            self.build()
        del self.attestation['files']['boot.img']
        self.receipts['userdata']['kind'] = 'couch-private-vendor-staging'
        self.write_receipts()
        with self.assertRaises(ValueError):
            self.build()
        self.assertFalse((self.root / 'output').exists())

    def test_source_commit_metadata_is_required_even_with_matching_archive_hash(self):
        for commit in ['b'*40, None]:
            data = self.git_archive(commit)
            (self.root / 'source.tar.gz').write_bytes(data)
            self.attestation['source_archive_sha256'] = hashlib.sha256(data).hexdigest()
            self.write_receipts()
            with self.assertRaisesRegex(ValueError, 'Git (commit differs|archive global commit metadata)'):
                self.build()
            self.assertFalse((self.root / 'output').exists())

    def test_oversized_pax_declaration_is_rejected_before_reading_its_body(self):
        item = tarfile.TarInfo('pax_global_header')
        item.type, item.size = tarfile.XGLTYPE, 2 * 1024**3
        data = gzip.compress(item.tobuf(format=tarfile.USTAR_FORMAT))
        with self.assertRaisesRegex(ValueError, 'bounded Git archive'):
            package.verify_source_commit(io.BytesIO(data), 'a'*40)

    def test_archive_is_flushed_before_link_and_json_outputs_before_return(self):
        events = []
        actual_link = os.link
        actual_sync = os.fsync
        def sync(fd):
            events.append('sync')
            return actual_sync(fd)
        def link(source, destination):
            self.assertEqual(events, ['sync'])
            events.append('link')
            return actual_link(source, destination)
        with patch.object(package.os, 'fsync', side_effect=sync), patch.object(package.os, 'link', side_effect=link):
            self.build()
        self.assertEqual(events, ['sync', 'link', 'sync', 'sync'])

    def test_source_archive_and_builder_receipt_bytes_must_match_attestation(self):
        source = self.root / 'source.tar.gz'
        source.write_bytes(b'changed source')
        with self.assertRaises(ValueError):
            self.build()
        source.write_bytes(self.source_bytes)
        self.receipts['ramdisk']['assembly'] = 'unattested receipt edit'
        self.save(self.root / 'ramdisk/ramdisk.json', self.receipts['ramdisk'])
        with self.assertRaises(ValueError):
            self.build()
        self.assertFalse((self.root / 'output').exists())

    def test_symlinks_output_collisions_and_live_input_mutation_preserve_existing_files(self):
        target = self.root / 'boot/zImage'
        original = target.read_bytes()
        target.unlink()
        (self.root / 'elsewhere').write_bytes(original)
        target.symlink_to(self.root / 'elsewhere')
        with self.assertRaises(ValueError):
            self.build()
        target.unlink()
        target.write_bytes(original)
        (self.root / 'output').mkdir()
        (self.root / 'output/keep').write_text('keep')
        with self.assertRaises(ValueError):
            self.build()
        self.assertEqual((self.root / 'output/keep').read_text(), 'keep')
        original_verify = package.verify_input
        def mutate_after_verify(source, expected):
            original_verify(source, expected)
            if source.name == target or expected == self.files['zImage']:
                target.write_bytes(bytes(len(original)))
        with patch.object(package, 'verify_input', side_effect=mutate_after_verify), self.assertRaisesRegex(ValueError, 'checksum changed'):
            self.build('mutated')
        self.assertFalse(list((self.root / 'mutated').glob('*.tar.gz')))

    @unittest.skipUnless(os.environ.get('COUCH_NATIVE_PUBLIC_HOST'), 'set verified native host path for offline admission')
    def test_native_host_admits_packager_output_without_network_or_device_access(self):
        result = self.build()
        directory = self.root / 'output'
        archive = directory / result['payload']['url'].rsplit('/', 1)[-1]
        output = subprocess.check_output([os.environ['COUCH_NATIVE_PUBLIC_HOST'], 'verify-public',
                                          str(directory / 'installer.json'), str(archive), str(self.root / 'native')], timeout=30)
        self.assertEqual(json.loads(output), {'verified_public_files': 6, 'device_access': False})


if __name__ == '__main__':
    unittest.main()
