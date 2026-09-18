"""The retained closure archive: determinism, restore, tamper and pin binding."""
import contextlib
import gzip
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import os_baseline
import package_closure
import prepare_rootfs
from clean_stage import StageError, checksum
from os_baseline import archive_pin, check_archive
from package_closure import IMAGE, PREFIX, archive, inventory, restore, verify


def closure(root, package=b'fixture-package'):
    """A minimal prepared closure directory, laid out like the real tool's."""
    for name in ('packages', 'indexes', 'keys'):
        (root / name).mkdir()
    (root / 'packages/musl-1.2.5-r11.apk').write_bytes(package)
    (root / 'indexes/APKINDEX.test.tar.gz').write_bytes(b'fixture-index')
    (root / 'keys/alpine.pub').write_bytes(b'fixture-public-key')
    (root / 'package-urls.txt').write_text(PREFIX + 'main/armv7/musl-1.2.5-r11.apk\n')
    manifest = inventory(root, ['musl'], IMAGE)
    (root / 'closure.json').write_text(json.dumps(manifest, sort_keys=True, indent=2) + '\n')
    return manifest


def pack(target, entries):
    """Write an archive directly, the way anyone repackaging one would."""
    with target.open('xb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as bundle:
                for item, data in entries:
                    item.size = len(data) if item.isreg() else 0
                    bundle.addfile(item, io.BytesIO(data) if item.size else None)
    return target


def regular(name, data):
    item = tarfile.TarInfo(name)
    item.mode, item.uid, item.gid, item.mtime = 0o644, 0, 0, 0
    return item, data


def unpack(source):
    with tarfile.open(source, mode='r:gz') as bundle:
        return [(member, bundle.extractfile(member).read()) for member in bundle]


class ClosureArchiveTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.closure = self.root / 'closure'
        self.closure.mkdir()
        self.manifest = closure(self.closure)

    def archived(self, name='retained.tar.gz'):
        return archive(self.closure, self.root / name)

    # Deterministic archiving.

    def test_archiving_the_same_closure_twice_gives_identical_bytes(self):
        self.archived('first.tar.gz')
        first = (self.root / 'first.tar.gz').read_bytes()
        # Metadata the archive must normalize away, not carry into its bytes.
        for path in sorted(self.closure.rglob('*')):
            if path.is_file():
                path.chmod(0o600)
                os.utime(path, (1, 1))
        self.archived('second.tar.gz')
        self.assertEqual(first, (self.root / 'second.tar.gz').read_bytes())

    def test_archive_entries_are_sorted_and_normalized(self):
        self.archived()
        members = [member for member, _ in unpack(self.root / 'retained.tar.gz')]
        names = [member.name for member in members]
        self.assertEqual(names, sorted(names))
        self.assertEqual(set(names), {*self.manifest['files'], 'closure.json'})
        for member in members:
            self.assertTrue(member.isreg())
            self.assertEqual((member.mode, member.uid, member.gid, member.mtime), (0o644, 0, 0, 0))

    def test_archive_verifies_the_closure_first_and_never_overwrites(self):
        (self.closure / 'packages/musl-1.2.5-r11.apk').write_bytes(b'tampered')
        with self.assertRaisesRegex(ValueError, 'changed'):
            self.archived()
        self.assertFalse((self.root / 'retained.tar.gz').exists())
        (self.closure / 'packages/musl-1.2.5-r11.apk').write_bytes(b'fixture-package')
        self.archived()
        kept = (self.root / 'retained.tar.gz').read_bytes()
        with self.assertRaises(FileExistsError):
            self.archived()
        self.assertEqual((self.root / 'retained.tar.gz').read_bytes(), kept)

    def test_a_failed_archive_leaves_no_half_written_file(self):
        with patch.object(package_closure, 'tarfile') as broken:
            broken.open.side_effect = OSError('disk full')
            with self.assertRaises(OSError):
                self.archived()
        self.assertFalse((self.root / 'retained.tar.gz').exists())

    # Restore round trip.

    def test_restore_reproduces_a_verified_closure_directory(self):
        self.archived()
        restored = restore(self.root / 'retained.tar.gz', self.root / 'restored')
        self.assertEqual(restored, self.manifest)
        self.assertEqual(verify(self.root / 'restored'), self.manifest)
        for name in {*self.manifest['files'], 'closure.json'}:
            self.assertEqual((self.root / 'restored' / name).read_bytes(),
                             (self.closure / name).read_bytes())
        # The round trip is closed: re-archiving the restored tree is identical.
        archive(self.root / 'restored', self.root / 'again.tar.gz')
        self.assertEqual((self.root / 'retained.tar.gz').read_bytes(),
                         (self.root / 'again.tar.gz').read_bytes())

    def test_restore_output_must_be_new_and_the_source_a_plain_file(self):
        self.archived()
        (self.root / 'taken').mkdir()
        with self.assertRaisesRegex(ValueError, 'must be new'):
            restore(self.root / 'retained.tar.gz', self.root / 'taken')
        with self.assertRaisesRegex(ValueError, 'regular file'):
            restore(self.closure, self.root / 'other')
        (self.root / 'link.tar.gz').symlink_to(self.root / 'retained.tar.gz')
        with self.assertRaisesRegex(ValueError, 'regular file'):
            restore(self.root / 'link.tar.gz', self.root / 'third')

    # Tamper detection inside the archive.

    def test_a_tampered_package_inside_a_repacked_archive_is_rejected(self):
        self.archived()
        entries = [(member, b'tampered' if member.name.endswith('.apk') else data)
                   for member, data in unpack(self.root / 'retained.tar.gz')]
        pack(self.root / 'repacked.tar.gz', entries)
        # The outer archive is self-consistent; closure.json still is not.
        with self.assertRaisesRegex(ValueError, 'changed'):
            restore(self.root / 'repacked.tar.gz', self.root / 'restored')

    def test_rewriting_the_manifest_to_match_a_tampered_package_loses_the_baseline_pin(self):
        self.archived()
        forged = self.root / 'forged'
        forged.mkdir()
        closure(forged, package=b'tampered')
        entries = [(member, (forged / member.name).read_bytes())
                   for member, _ in unpack(self.root / 'retained.tar.gz')]
        pack(self.root / 'forged.tar.gz', entries)
        # A rewritten manifest satisfies the inventory, so the archive cannot be
        # the only binding: the reviewed manifest identity is what refuses it.
        self.assertNotEqual(restore(self.root / 'forged.tar.gz', self.root / 'restored'), self.manifest)
        honest = checksum((self.closure / 'closure.json').read_bytes())
        self.assertNotEqual(checksum((self.root / 'restored/closure.json').read_bytes()), honest)
        with self.assertRaises(StageError):
            check_archive((self.root / 'forged.tar.gz').read_bytes(), pin=self.pin())

    # Hostile archive entries.

    def test_restore_refuses_unsafe_duplicate_or_nonregular_entries(self):
        link = tarfile.TarInfo('keys/escape')
        link.type, link.linkname = tarfile.SYMTYPE, '/etc/passwd'
        folder = tarfile.TarInfo('packages')
        folder.type = tarfile.DIRTYPE
        cases = {
            'absolute': [regular('/etc/passwd', b'x')],
            'traversal': [regular('../escape', b'x')],
            'interior traversal': [regular('keys/../../escape', b'x')],
            'current': [regular('./closure.json', b'x')],
            'backslash': [regular('keys\\escape', b'x')],
            'duplicate': [regular('keys/alpine.pub', b'x'), regular('keys/alpine.pub', b'y')],
            'symlink': [(link, b'')],
            'directory': [(folder, b'')],
        }
        for label, entries in cases.items():
            with self.subTest(entry=label):
                target = self.root / (label.replace(' ', '-') + '.tar.gz')
                pack(target, entries)
                with self.assertRaises(ValueError):
                    restore(target, self.root / label.replace(' ', '-'))

    def test_restore_rejects_an_oversized_source_before_reading_or_writing(self):
        # A release asset is bounded by its own size first, so a hostile archive
        # never reaches the decompressor or creates an output directory.
        huge = self.root / 'huge.tar.gz'
        huge.write_bytes(b'not an archive at all')
        with patch.object(package_closure, 'ARCHIVE_LIMIT', 4):
            with self.assertRaisesRegex(ValueError, 'too large'):
                restore(huge, self.root / 'huge')
        self.assertFalse((self.root / 'huge').exists())

    def test_restore_bounds_entry_count_and_size_before_writing(self):
        self.archived()
        source = self.root / 'retained.tar.gz'
        with patch.object(package_closure, 'MEMBER_COUNT', 1):
            with self.assertRaisesRegex(ValueError, 'too many'):
                restore(source, self.root / 'a')
        with patch.object(package_closure, 'MEMBER_LIMIT', 1):
            with self.assertRaisesRegex(ValueError, 'too large'):
                restore(source, self.root / 'b')
        with patch.object(package_closure, 'ARCHIVE_LIMIT', 1):
            with self.assertRaisesRegex(ValueError, 'too large'):
                restore(source, self.root / 'c')

    # Baseline pin binding.

    def pin(self, **changes):
        data = (self.root / 'retained.tar.gz').read_bytes()
        value = dict(schema=1, model='sanytron-ha100', id='baseline-fixture',
                     package_closure_sha256=checksum((self.closure / 'closure.json').read_bytes()),
                     runtime_boot_sha256='b' * 64,
                     package_closure_archive={'file': 'retained.tar.gz', 'size': len(data),
                                              'sha256': checksum(data)})
        value.update(changes)
        return value

    def test_only_the_pinned_archive_bytes_are_accepted(self):
        self.archived()
        data = (self.root / 'retained.tar.gz').read_bytes()
        self.assertEqual(check_archive(data, pin=self.pin())['size'], len(data))
        for label, pin in {
                'digest': self.pin(package_closure_archive={
                    'file': 'retained.tar.gz', 'size': len(data), 'sha256': 'c' * 64}),
                'size': self.pin(package_closure_archive={
                    'file': 'retained.tar.gz', 'size': len(data) + 1, 'sha256': checksum(data)}),
                'unpinned': self.pin(package_closure_archive=None)}.items():
            with self.subTest(pin=label), self.assertRaises(StageError):
                check_archive(data, pin=pin)
        with self.assertRaisesRegex(StageError, 'must state'):
            check_archive(data, pin={'schema': 1})

    def test_archive_pin_shape_is_validated(self):
        self.archived()
        good = self.pin()['package_closure_archive']
        for label, value in {
                'extra field': {**good, 'url': 'https://example.invalid/x.tar.gz'},
                'missing field': {'file': good['file'], 'sha256': good['sha256']},
                'absolute file': {**good, 'file': '/tmp/x.tar.gz'},
                'traversal file': {**good, 'file': '../x.tar.gz'},
                'wrong suffix': {**good, 'file': 'retained.zip'},
                'zero size': {**good, 'size': 0},
                'boolean size': {**good, 'size': True},
                'string size': {**good, 'size': str(good['size'])},
                'short digest': {**good, 'sha256': 'ab'},
                'uppercase digest': {**good, 'sha256': good['sha256'].upper()}}.items():
            with self.subTest(pin=label), self.assertRaises(StageError):
                archive_pin(self.pin(package_closure_archive=value))
        self.assertEqual(archive_pin(self.pin()), good)

    def test_shipped_baseline_declares_its_retained_archive(self):
        pin = json.loads(os_baseline.PIN.read_text())
        self.assertIn('package_closure_archive', pin)
        self.assertEqual(archive_pin(), pin['package_closure_archive'])
        if pin['package_closure_archive'] is None:
            # An absent pin refuses archive input; it never accepts any bytes.
            with self.assertRaisesRegex(StageError, 'No reviewed retained closure archive'):
                check_archive(b'anything')

    # Assembly input dispatch.

    def test_assembly_accepts_a_directory_or_the_pinned_archive_only(self):
        self.archived()
        pinned = self.root / 'baseline.json'
        pinned.write_text(json.dumps(self.pin()))
        with prepare_rootfs.ExitStack() as stack:
            self.assertEqual(prepare_rootfs.open_closure(self.closure, stack), self.closure)
        with patch.object(os_baseline, 'PIN', pinned):
            with prepare_rootfs.ExitStack() as stack:
                restored = prepare_rootfs.open_closure(self.root / 'retained.tar.gz', stack)
                self.assertEqual(verify(restored), self.manifest)
                live = restored
            self.assertFalse(live.exists())  # The restored copy is not left behind.
            entries = [(member, b'tampered' if member.name.endswith('.apk') else data)
                       for member, data in unpack(self.root / 'retained.tar.gz')]
            pack(self.root / 'repacked.tar.gz', entries)
            with prepare_rootfs.ExitStack() as stack, self.assertRaises(StageError):
                prepare_rootfs.open_closure(self.root / 'repacked.tar.gz', stack)
        with prepare_rootfs.ExitStack() as stack, self.assertRaisesRegex(StageError, 'prepared directory'):
            prepare_rootfs.open_closure(self.root / 'absent.tar.gz', stack)

    def test_assembly_refuses_a_closure_the_baseline_never_reviewed(self):
        with self.assertRaisesRegex(StageError, 'reviewed package closure'):
            prepare_rootfs.assemble({}, self.closure, self.root / 'staged')

    # Authentication is not skippable through the archive.

    def docker(self, *argv):
        noise = io.StringIO()
        with patch.object(package_closure, 'subprocess') as shell:
            with (patch('sys.argv', ['package_closure.py', *argv]),
                  contextlib.redirect_stdout(noise), contextlib.redirect_stderr(noise)):
                package_closure.main()
            return [call.args[0] for call in shell.run.call_args_list]

    def test_restore_still_authenticates_offline_and_never_runs_docker_alone(self):
        self.archived()
        source = str(self.root / 'retained.tar.gz')
        self.assertEqual(self.docker('restore', str(self.root / 'quiet'), '--archive', source), [])
        commands = self.docker('restore', str(self.root / 'checked'), '--archive', source, '--authenticate')
        self.assertEqual(len(commands), 1)
        self.assertIn('--network=none', commands[0])
        self.assertIn('/check.sh', commands[0])
        self.assertIn(f'type=bind,src={(self.root / "checked").resolve()},dst=/out,readonly', commands[0])
        self.assertEqual(commands[0][-3:], [IMAGE, 'sh', '/check.sh'])

    def test_archive_and_restore_require_an_archive_path(self):
        for operation in ('archive', 'restore'):
            with self.subTest(operation=operation), self.assertRaises(SystemExit):
                self.docker(operation, str(self.closure))


if __name__ == '__main__':
    unittest.main()
