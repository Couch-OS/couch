import io
import tarfile
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from clean_stage import GENERATED, StageError
from prepare_rootfs import normalize, prepare, REVIEWED_SET_ID


class PackagedRootfsTests(unittest.TestCase):
    def test_old_closure_fails_before_container_or_output_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'closure.json').write_text('{}')
            with patch('prepare_rootfs.verify', return_value={}), patch('prepare_rootfs.subprocess.run') as run:
                with self.assertRaisesRegex(StageError, 'reviewed FFmpeg package closure'):
                    prepare({}, root, root / 'output')
                run.assert_not_called()
            self.assertFalse((root / 'output').exists())

    def fixture(self, extra=(), defaults=True, stamp=1):
        output = io.BytesIO()
        items = [(name, data, tarfile.REGTYPE, '') for name, data in GENERATED.items()] if defaults else []
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, data, kind, target in [*items, *extra]:
                item = tarfile.TarInfo(name)
                item.type, item.linkname, item.mtime = kind, target, stamp
                item.mode, item.uid, item.gid = 0o755, 123, 456
                item.size = len(data) if kind == tarfile.REGTYPE else 0
                archive.addfile(item, io.BytesIO(data) if item.size else None)
        return output.getvalue()

    def test_normalizes_times_and_order_but_preserves_package_ownership(self):
        first, count = normalize(self.fixture(stamp=1), 1234)
        second, _ = normalize(self.fixture(stamp=9999999), 1234)
        self.assertEqual(first, second)
        self.assertEqual(count, len(GENERATED))
        with tarfile.open(fileobj=io.BytesIO(first)) as archive:
            members = archive.getmembers()
            self.assertEqual([m.name for m in members], sorted(GENERATED))
            self.assertTrue(all(m.uid == 123 and m.gid == 456 and m.mtime == 1234 for m in members))

    def test_rejects_runtime_private_state_after_package_scripts(self):
        for path in ('root/.ssh/authorized_keys', 'etc/ssh/ssh_host_rsa_key', 'etc/machine-id',
                     'opt/couch/networks.conf', 'dev/null'):
            with self.subTest(path=path), self.assertRaises(StageError):
                normalize(self.fixture([(path, b'private', tarfile.REGTYPE, '')]), 1234)

    def test_rejects_link_ancestor_and_archive_escape(self):
        for item in [('opt/couch', b'', tarfile.SYMTYPE, '/tmp'),
                     ('../escape', b'x', tarfile.REGTYPE, ''),
                     ('escape', b'', tarfile.SYMTYPE, '../outside')]:
            with self.assertRaises(StageError):
                normalize(self.fixture([item]), 1234)

    def test_missing_clean_defaults_and_passwords_fail_closed(self):
        with self.assertRaisesRegex(StageError, 'clean defaults'):
            normalize(self.fixture(defaults=False), 1234)
        with self.assertRaisesRegex(StageError, 'password credentials'):
            normalize(self.fixture([('etc/shadow', b'root:$6$hash:::::::\n', tarfile.REGTYPE, '')]), 1234)
        with self.assertRaises(StageError):
            normalize(self.fixture([('etc/shadow', b'malformed\n', tarfile.REGTYPE, '')]), 1234)

    def set_id_fixture(self, name='usr/libexec/dbus-daemon-launch-helper',
                       content=b'reviewed helper', mode=0o4750, uid=0, gid=101,
                       kind=tarfile.REGTYPE, group=b'messagebus:x:101:\n'):
        output = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(self.fixture()), mode='r:gz') as source:
            with tarfile.open(fileobj=output, mode='w:gz') as target:
                for member in source:
                    target.addfile(member, source.extractfile(member))
                member = tarfile.TarInfo('etc/group')
                member.size = len(group)
                target.addfile(member, io.BytesIO(group))
                member = tarfile.TarInfo(name)
                member.type, member.mode, member.uid, member.gid = kind, mode, uid, gid
                member.size = len(content) if kind == tarfile.REGTYPE else 0
                member.linkname = 'bin/unreviewed' if kind == tarfile.SYMTYPE else ''
                target.addfile(member, io.BytesIO(content) if member.size else None)
        return output.getvalue()

    def test_reviewed_helper_retains_set_id_and_requires_exact_identity(self):
        from clean_stage import checksum
        name = 'usr/libexec/dbus-daemon-launch-helper'
        reviewed = (checksum(b'reviewed helper'), 0o4750, 0, 101)
        with patch.dict(REVIEWED_SET_ID, {name: reviewed}, clear=True):
            normalized, _ = normalize(self.set_id_fixture(), 1234)
            with tarfile.open(fileobj=io.BytesIO(normalized)) as archive:
                helper = archive.getmember(name)
                self.assertEqual((helper.mode, helper.uid, helper.gid), (0o4750, 0, 101))
            for change in ({'name': 'usr/bin/unreviewed'}, {'content': b'changed'},
                           {'mode': 0o6750}, {'uid': 1}, {'gid': 102},
                           {'kind': tarfile.SYMTYPE}):
                with self.subTest(change=change), self.assertRaisesRegex(StageError, 'Set-ID'):
                    normalize(self.set_id_fixture(**change), 1234)
            for group in (b'other:x:101:\n', b'messagebus:x:102:\n',
                          b'messagebus:x:102:\nmessagebus:x:101:\n',
                          b'messagebus:x:101:\nmessagebus:x:101:\n',
                          b'messagebus:x:101:\nother:x:101:\n'):
                with self.subTest(group=group), self.assertRaisesRegex(StageError, 'messagebus group'):
                    normalize(self.set_id_fixture(group=group), 1234)


if __name__ == '__main__':
    unittest.main()
