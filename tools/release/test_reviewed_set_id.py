"""The reviewed set-ID allowlist admits exactly one file and refuses the rest."""
import tarfile
import unittest
from unittest.mock import patch

from clean_stage import REVIEWED_SET_ID, checksum, reviewed_set_id

HELPER = 'usr/libexec/dbus-daemon-launch-helper'
REVIEWED = b'reviewed helper bytes'
OTHER = b'some other program'


def member(mode, data=REVIEWED, kind=tarfile.REGTYPE):
    item = tarfile.TarInfo('ignored')
    item.mode, item.size, item.type = mode, len(data), kind
    return item


class ShippedAllowlist(unittest.TestCase):
    """The table in the repository names one file, with the reviewed values."""

    def test_only_the_dbus_helper_is_listed(self):
        self.assertEqual(set(REVIEWED_SET_ID), {HELPER})

    def test_the_entry_pins_mode_package_and_digest(self):
        self.assertEqual(REVIEWED_SET_ID[HELPER], {
            'package': 'dbus-daemon-launch-helper-1.14.10-r4',
            'mode': 0o4750,
            'sha256': '26334163ead6299bdcb3f8b2e72ae3033cd80094ecdf731d7f934aaf623c782a'})


class ReviewedSetId(unittest.TestCase):
    """Path, mode and bytes must all match the reviewed entry."""

    def setUp(self):
        table = {HELPER: {'package': 'dbus-daemon-launch-helper-1.14.10-r4',
                          'mode': 0o4750, 'sha256': checksum(REVIEWED)}}
        patcher = patch.dict(REVIEWED_SET_ID, table, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_the_reviewed_file_passes(self):
        self.assertTrue(reviewed_set_id(HELPER, member(0o4750), REVIEWED))

    def test_a_different_set_id_file_is_refused(self):
        self.assertFalse(reviewed_set_id('usr/bin/sudo', member(0o4755, OTHER), OTHER))

    def test_the_reviewed_path_with_other_bytes_is_refused(self):
        self.assertFalse(reviewed_set_id(HELPER, member(0o4750, OTHER), OTHER))

    def test_the_reviewed_path_with_another_mode_is_refused(self):
        # 0o0750 carries no set-ID bits: being listed never blesses a path.
        for mode in (0o4755, 0o6750, 0o2750, 0o0750):
            with self.subTest(mode=oct(mode)):
                self.assertFalse(reviewed_set_id(HELPER, member(mode), REVIEWED))

    def test_a_link_or_directory_on_the_reviewed_path_is_refused(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE):
            with self.subTest(kind=kind):
                self.assertFalse(
                    reviewed_set_id(HELPER, member(0o4750, kind=kind), REVIEWED))

