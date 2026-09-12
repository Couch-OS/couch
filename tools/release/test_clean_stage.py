import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from clean_stage import artifact_destination, build, StageError


class CleanStageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def fixture(self, extra=()):
        path = self.root / 'base.tar.gz'
        with tarfile.open(path, 'w:gz') as archive:
            for name, data, kind, link in [('etc/alpine-release', b'3.21.7\n', tarfile.REGTYPE, ''),
                    ('bin/busybox', b'\x7fELF\x01\x01'+b'\0'*12+b'\x28\0', tarfile.REGTYPE, ''),
                    *([('etc/shadow', b'root:!:::::::\n', tarfile.REGTYPE, '')] if not any(x[0]=='etc/shadow' for x in extra) else []), *extra]:
                item = tarfile.TarInfo(name)
                item.type, item.linkname, item.mode = kind, link, 0o644
                item.size = len(data) if kind == tarfile.REGTYPE else 0
                archive.addfile(item, io.BytesIO(data) if item.size else None)
        return {'schema': 1, 'source_commit': 'a'*40, 'source_date_epoch': 1234567890,
                'alpine': {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                           'version': '3.21.7', 'architecture': 'armv7'}, 'artifacts': []}

    def test_sonos_is_packaged_at_the_runtime_path_with_pinned_content(self):
        source = self.root / 'sonos'
        source.write_bytes(b'static ARM fixture')
        spec = self.fixture()
        item = {'source': 'sonos', 'destination': 'opt/couch/couch-sonos',
                'mode': 0o755, 'sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
        spec['artifacts'] = [item]
        data, manifest = build(spec, self.root)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            self.assertEqual(archive.extractfile(item['destination']).read(), source.read_bytes())
            self.assertEqual(archive.getmember(item['destination']).mode, 0o755)
        self.assertEqual(manifest['artifacts'][0]['sha256'], item['sha256'])
        source.write_bytes(b'changed')
        with self.assertRaisesRegex(StageError, 'checksum mismatch'):
            build(spec, self.root)

    def test_coreelec_is_packaged_at_the_runtime_path_with_pinned_content(self):
        source = self.root / 'coreelec'
        source.write_bytes(b'static ARM fixture')
        spec = self.fixture()
        item = {'source': 'coreelec', 'destination': 'opt/couch/couch-coreelec',
                'mode': 0o755, 'sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
        spec['artifacts'] = [item]
        data, manifest = build(spec, self.root)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            self.assertEqual(archive.extractfile(item['destination']).read(), source.read_bytes())
            self.assertEqual(archive.getmember(item['destination']).mode, 0o755)
        self.assertEqual(manifest['artifacts'][0]['sha256'], item['sha256'])
        source.write_bytes(b'changed')
        with self.assertRaisesRegex(StageError, 'checksum mismatch'):
            build(spec, self.root)

    def test_only_named_recovery_cgi_scripts_are_allowed(self):
        for name in ('save', 'setpw', 'scan', 'enroll'):
            self.assertEqual(artifact_destination('opt/couch/www/cgi-bin/' + name),
                             'opt/couch/www/cgi-bin/' + name)
        for name in ('debug', 'authorized_keys', '../enroll', 'nested/enroll'):
            with self.assertRaises(StageError):
                artifact_destination('opt/couch/www/cgi-bin/' + name)

    def test_cgi_script_requires_executable_mode(self):
        source = self.root / 'scan'; source.write_bytes(b'#!/bin/busybox sh\n')
        spec = self.fixture()
        spec['artifacts'] = [{'source': 'scan', 'destination': 'opt/couch/www/cgi-bin/scan',
                              'sha256': hashlib.sha256(source.read_bytes()).hexdigest(), 'mode': 0o644}]
        with self.assertRaisesRegex(StageError, 'executable'):
            build(spec, self.root)
        spec['artifacts'][0]['mode'] = 0o755
        build(spec, self.root)

    def test_reproducible_and_empty_onboarding_config(self):
        spec = self.fixture()
        first, manifest = build(spec, self.root)
        second, _ = build(spec, self.root)
        self.assertEqual(first, second)
        self.assertFalse(manifest['installable'])
        with tarfile.open(fileobj=io.BytesIO(first)) as archive:
            self.assertEqual(json.load(archive.extractfile('opt/couch/config.json')), {'schema_version': 1})
            self.assertEqual(archive.getmember('opt/couch').uid, 0)
            self.assertEqual(archive.getmember('opt/couch').mtime, spec['source_date_epoch'])

    def test_rejects_private_state(self):
        for name in ['root/.ssh/authorized_keys', 'etc/ssh/ssh_host_ed25519_key',
                     'opt/couch/networks.conf', 'opt/couch/config.json', 'etc/machine-id']:
            with self.subTest(name=name):
                with self.assertRaises(StageError):
                    build(self.fixture([(name, b'private', tarfile.REGTYPE, '')]), self.root)

    def test_rejects_archive_traversal_and_link_ancestors(self):
        cases = [[('../escape', b'x', tarfile.REGTYPE, '')],
                 [('opt', b'', tarfile.SYMTYPE, '/tmp/outside')]]
        for extra in cases:
            with self.assertRaises(StageError):
                build(self.fixture(extra), self.root)

    def test_allows_native_absolute_busybox_symlinks_without_following(self):
        data, _ = build(self.fixture([('bin/sh', b'', tarfile.SYMTYPE, '/bin/busybox')]), self.root)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            self.assertEqual(archive.getmember('bin/sh').linkname, '/bin/busybox')

    def test_artifacts_require_hash_and_cannot_supply_config(self):
        source = self.root / 'gui'; source.write_bytes(b'example artifact')
        item = {'source': 'gui', 'destination': 'opt/couch/couch-gui',
                'sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
        spec = self.fixture(); spec['artifacts'] = [item]
        build(spec, self.root)
        item['sha256'] = '0'*64
        with self.assertRaises(StageError): build(spec, self.root)
        item['destination'] = 'opt/couch/config.json'
        with self.assertRaises(StageError): build(spec, self.root)

    def test_rejects_source_escape_and_private_key_material(self):
        source = self.root / 'gui'; source.write_bytes(b'-----BEGIN PRIVATE KEY-----')
        item = {'source': 'gui', 'destination': 'opt/couch/couch-gui',
                'sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
        spec = self.fixture(); spec['artifacts'] = [item]
        with self.assertRaises(StageError): build(spec, self.root)
        item['source'] = '/etc/passwd'
        with self.assertRaises(StageError): build(spec, self.root)

    def test_rejects_passwords_and_wrong_base_version(self):
        with self.assertRaisesRegex(StageError, 'password credentials'):
            build(self.fixture([('etc/shadow', b'root:$6$hash:::::::\n', tarfile.REGTYPE, '')]), self.root)
        spec = self.fixture(); spec['alpine']['version'] = '3.21.8'
        with self.assertRaises(StageError): build(spec, self.root)


if __name__ == '__main__': unittest.main()
