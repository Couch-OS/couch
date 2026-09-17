from pathlib import Path
import tempfile
import unittest

import bump_version


class InstallerVersionTests(unittest.TestCase):
    def test_version_bump_only_changes_installer_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            version = root / 'VERSION'
            published = root / 'current-release.txt'
            published.write_text('published-os-version\n')
            bump_version.bump('v1.2.3-alpha.1', version)
            self.assertEqual(version.read_text(), 'v1.2.3-alpha.1\n')
            self.assertEqual(published.read_text(), 'published-os-version\n')
            for invalid in ('latest', 'installer-v1.2.3', 'v01.2.3', 'v1.2.3\nother'):
                with self.assertRaises(ValueError):
                    bump_version.bump(invalid, version)
                self.assertEqual(version.read_text(), 'v1.2.3-alpha.1\n')


if __name__ == '__main__':
    unittest.main()
