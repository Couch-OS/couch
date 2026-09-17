import io
import json
import tarfile
import unittest
from clean_stage import checksum, StageError
from os_baseline import seed, MARKER, PIN
from package_closure import DEFAULT_PACKAGES
from prepare_rootfs import normalize
import test_prepare_rootfs


class OsBaselineTests(unittest.TestCase):
    def fixture(self, boot=b'boot fixture', ffmpeg=None, marker=False):
        if ffmpeg is None:
            ffmpeg = b'\x7fELF\x01\x01' + bytes(12) + b'\x28\x00'
        extra = [('opt/couch/runtime-boot.sh', boot, tarfile.REGTYPE, ''),
                 ('usr/bin/ffmpeg', ffmpeg, tarfile.REGTYPE, '')]
        if marker:
            extra.append((MARKER, b'old', tarfile.REGTYPE, ''))
        return test_prepare_rootfs.PackagedRootfsTests().fixture(extra)

    def pin(self):
        return dict(schema=1, model='sanytron-ha100', id='baseline-fixture',
                    package_closure_sha256='a'*64, runtime_boot_sha256=checksum(b'boot fixture'))

    def test_reviewed_pin_matches_stable_boot_source(self):
        pin = json.loads(PIN.read_text())
        self.assertEqual(pin['runtime_boot_sha256'], checksum((PIN.parents[2] / 'stage2/runtime-boot.sh').read_bytes()))
        self.assertEqual(pin['model'], 'sanytron-ha100')
        self.assertRegex(pin['id'], r'^[a-z0-9-]{1,96}$')

    def test_reviewed_package_manifest_matches_pin_and_runtime_requirements(self):
        pin = json.loads(PIN.read_text())
        data = PIN.with_name('ha100_os_packages.json').read_bytes()
        self.assertEqual(checksum(data), pin['package_closure_sha256'])
        manifest = json.loads(data)
        self.assertEqual(manifest['architecture'], 'armv7')
        roots = {request.split('=', 1)[0] for request in manifest['requested']}
        self.assertFalse(set(DEFAULT_PACKAGES) - roots,
                         'Reviewed package baseline must cover current runtime requirements')

    def test_only_matching_package_and_boot_inputs_seed_normalized_marker(self):
        data, marker = seed(self.fixture(), 'a'*64, pin=self.pin())
        normalized, _ = normalize(data, 1234)
        with tarfile.open(fileobj=io.BytesIO(normalized)) as archive:
            entry = archive.getmember(MARKER)
            self.assertEqual(entry.mode, 0o644)
            self.assertEqual(json.load(archive.extractfile(entry)), marker)
            self.assertEqual(marker, dict(schema=1, model='sanytron-ha100', id='baseline-fixture'))

    def prefixed(self, data, duplicate=False):
        output = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(data)) as source:
            with tarfile.open(fileobj=output, mode='w') as target:
                for member in source:
                    member.name = './' + member.name
                    target.addfile(member, source.extractfile(member) if member.isreg() else None)
                    if duplicate and member.name == './usr/bin/ffmpeg':
                        member.name = 'usr/bin/ffmpeg'
                        target.addfile(member, source.extractfile(member))
        return output.getvalue()

    def test_real_assembler_dot_prefixes_seed_and_normalize(self):
        data, marker = seed(self.prefixed(self.fixture()), 'a'*64, pin=self.pin())
        normalized, _ = normalize(data, 1234)
        with tarfile.open(fileobj=io.BytesIO(normalized)) as archive:
            self.assertEqual(json.load(archive.extractfile(MARKER)), marker)
            self.assertTrue(archive.getmember('usr/bin/ffmpeg').isreg())

    def test_alias_duplicates_and_prefixed_existing_marker_are_rejected(self):
        for data in [self.prefixed(self.fixture(), duplicate=True),
                     self.prefixed(self.fixture(marker=True))]:
            with self.assertRaises(StageError):
                seed(data, 'a'*64, pin=self.pin())

    def test_wrong_package_boot_architecture_or_existing_marker_are_refused(self):
        for data, digest in [(self.fixture(), 'b'*64), (self.fixture(boot=b'old boot'), 'a'*64),
                             (self.fixture(ffmpeg=b'wrong architecture'), 'a'*64),
                             (self.fixture(marker=True), 'a'*64)]:
            with self.assertRaises(StageError):
                seed(data, digest, pin=self.pin())


if __name__ == '__main__':
    unittest.main()
