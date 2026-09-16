import copy
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from clean_stage import GENERATED, StageError, checksum
import fresh_os


def binding_fixture():
    baseline = json.loads(fresh_os.PIN.read_text())
    return dict(schema=1, kind='couch-fresh-os-core-binding', installable=False,
                source_commit='a' * 40, rootfs_archive_sha256='b' * 64,
                runtime_inventory_sha256='c' * 64, runtime_build_receipt_sha256='d' * 64,
                integration_receipt_sha256='e' * 64,
                official_integration_key_sha256=json.loads(fresh_os.DEFAULT.read_text())['feed']['public_key']['sha256'],
                runtime_boot_sha256=baseline['runtime_boot_sha256'],
                package_closure_sha256=baseline['package_closure_sha256'],
                os_baseline={key: baseline[key] for key in ('schema', 'model', 'id')},
                core_files={name: dict(size=20, sha256='f' * 64) for name in fresh_os.CORE},
                tested_integration_set=dict(candidate_commit='a' * 40, artifact_bytes_verified=True,
                    protocol_version=1, rollout=dict(bundle_packages_in_runtime=False,
                    bundle_packages_in_installer=False, automatic_install=False,
                    automatic_configuration_migration=False)))


class FreshOsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binding = binding_fixture()
        key = (fresh_os.DEFAULT.parents[2] / 'daemon/couch-integrations/src/official.rsa.pub').read_bytes()
        self.files = {**GENERATED, fresh_os.MARKER: json.dumps(self.binding['os_baseline']).encode(),
                      'opt/couch/runtime-boot.sh': (fresh_os.PIN.parents[2] / 'stage2/runtime-boot.sh').read_bytes()}
        for name in fresh_os.CORE:
            self.files['opt/couch/' + name] = b'fixture core ' + name.encode() + (key if name == 'couch-confd' else b'')
        artifacts = [dict(destination=name, source=name, mode=0o755, sha256=checksum(data))
                     for name, data in self.files.items() if name.startswith('opt/couch/')
                     and name not in GENERATED and name != fresh_os.MARKER]
        self.inventory = dict(kind='couch-runtime-payload-inventory', clean_runtime_ready=True,
                              tracked_payload_worktree_clean=True, inventory_source_commit='a' * 40,
                              tested_integration_set=self.binding['tested_integration_set'], artifacts=artifacts)
        self.build = dict(schema=1, kind='couch-unsigned-runtime-build', source_commit='a' * 40,
                          target='armv7-unknown-linux-musleabihf', files=[
                              dict(path=path, size=len(self.files['opt/couch/' + name]),
                                   sha256=checksum(self.files['opt/couch/' + name]))
                              for name, path in fresh_os.CORE.items()])
        self.manifest = dict(kind='couch-packaged-staging', installable=False, source_commit='a' * 40,
                             source_date_epoch=100, package_closure_sha256=self.binding['package_closure_sha256'])
        self.write()
        self.verifier = patch.object(fresh_os, 'verify_receipt', return_value=self.binding['tested_integration_set'])
        self.verifier.start()
        self.addCleanup(self.verifier.stop)

    def write(self, link=None):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, data in self.files.items():
                entry = tarfile.TarInfo(name)
                entry.size, entry.mode = len(data), 0o755
                if name == link:
                    entry.type, entry.size, entry.linkname = tarfile.SYMTYPE, 0, '/tmp/old-core'
                archive.addfile(entry, io.BytesIO(data) if entry.isreg() else None)
        raw = output.getvalue()
        self.manifest['archive_sha256'] = checksum(raw)
        (self.root / 'rootfs-staging.tar.gz').write_bytes(raw)
        for name, value in [('staging.json', self.manifest), ('inventory.json', self.inventory),
                            ('build.json', self.build), ('integration.json', self.binding['tested_integration_set'])]:
            (self.root / name).write_text(json.dumps(value))

    def bind(self):
        return fresh_os.bind(self.root, self.root / 'inventory.json', self.root / 'build.json',
                             self.root / 'integration.json')

    def test_exact_core_bootstrap_key_and_receipts_are_bound(self):
        result = self.bind()
        self.assertEqual(result['rootfs_archive_sha256'], self.manifest['archive_sha256'])
        self.assertEqual(result['runtime_inventory_sha256'], checksum((self.root / 'inventory.json').read_bytes()))
        self.assertEqual(result['core_files']['couch-confd']['sha256'], checksum(self.files['opt/couch/couch-confd']))
        self.assertFalse(result['installable'])

    def test_old_core_cannot_be_relabelled_even_if_inventory_is_rewritten(self):
        self.files['opt/couch/couch-confd'] = b'old core bytes'
        for artifact in self.inventory['artifacts']:
            if artifact['destination'] == 'opt/couch/couch-confd':
                artifact['sha256'] = checksum(b'old core bytes')
        self.write()
        with self.assertRaisesRegex(StageError, 'original build receipt'):
            self.bind()

    def test_old_bootstrap_with_current_baseline_id_and_rewritten_inventory_is_rejected(self):
        self.files['opt/couch/runtime-boot.sh'] = b'old bootstrap without BCB-clear fix'
        for artifact in self.inventory['artifacts']:
            if artifact['destination'] == 'opt/couch/runtime-boot.sh':
                artifact['sha256'] = checksum(self.files[artifact['destination']])
        self.write()
        with self.assertRaisesRegex(StageError, 'corrected stable bootstrap'):
            self.bind()

    def test_matching_old_build_receipt_without_official_key_is_rejected(self):
        self.files['opt/couch/couch-confd'] = b'old core bytes'
        for artifact in self.inventory['artifacts']:
            if artifact['destination'] == 'opt/couch/couch-confd':
                artifact['sha256'] = checksum(b'old core bytes')
        for item in self.build['files']:
            if item['path'] == fresh_os.CORE['couch-confd']:
                item.update(size=len(b'old core bytes'), sha256=checksum(b'old core bytes'))
        self.write()
        with self.assertRaisesRegex(StageError, 'does not embed'):
            self.bind()

    def test_mixed_commit_and_unverified_receipt_are_rejected(self):
        self.build['source_commit'] = 'b' * 40
        self.write()
        with self.assertRaisesRegex(StageError, 'same frozen core commit'):
            self.bind()
        self.build['source_commit'] = 'a' * 40
        self.binding['tested_integration_set']['artifact_bytes_verified'] = False
        self.write()
        with self.assertRaisesRegex(StageError, 'verified same-source'):
            self.bind()

    def test_package_slots_and_nonregular_core_are_rejected(self):
        self.files['opt/couch/integrations/denon/package.apk'] = b'package fixture'
        self.write()
        with self.assertRaisesRegex(StageError, 'installed integration'):
            self.bind()
        del self.files['opt/couch/integrations/denon/package.apk']
        self.write(link='opt/couch/couch-confd')
        with self.assertRaisesRegex(StageError, 'nonregular'):
            self.bind()

    def test_boundary_rejects_missing_binding_mixed_rootfs_and_bad_baseline(self):
        for value in [None, {}, {**self.binding, 'source_commit': 'b' * 40},
                      {**self.binding, 'runtime_boot_sha256': '0' * 64},
                      {**self.binding, 'rootfs_archive_sha256': '0' * 64}]:
            with self.subTest(value=value), self.assertRaises(StageError):
                fresh_os.validate_binding(value, 'a' * 40, 'b' * 64)
        altered = copy.deepcopy(self.binding)
        del altered['core_files']['couch-confd']
        with self.assertRaises(StageError):
            fresh_os.validate_binding(altered, 'a' * 40, 'b' * 64)


if __name__ == '__main__':
    unittest.main()
