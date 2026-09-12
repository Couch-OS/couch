import io
import json
import tarfile
import unittest
from clean_stage import checksum, StageError
from os_baseline import seed, MARKER, PIN
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

    def test_only_matching_package_and_boot_inputs_seed_normalized_marker(self):
        data, marker = seed(self.fixture(), 'a'*64, pin=self.pin())
        normalized, _ = normalize(data, 1234)
        with tarfile.open(fileobj=io.BytesIO(normalized)) as archive:
            entry = archive.getmember(MARKER)
            self.assertEqual(entry.mode, 0o644)
            self.assertEqual(json.load(archive.extractfile(entry)), marker)
            self.assertEqual(marker, dict(schema=1, model='sanytron-ha100', id='baseline-fixture'))

    def test_wrong_package_boot_architecture_or_existing_marker_are_refused(self):
        for data, digest in [(self.fixture(), 'b'*64), (self.fixture(boot=b'old boot'), 'a'*64),
                             (self.fixture(ffmpeg=b'wrong architecture'), 'a'*64),
                             (self.fixture(marker=True), 'a'*64)]:
            with self.assertRaises(StageError):
                seed(data, digest, pin=self.pin())


if __name__ == '__main__':
    unittest.main()
