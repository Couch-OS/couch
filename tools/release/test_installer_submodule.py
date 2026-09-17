import subprocess
import unittest

import installer_pins


class InstallerSubmoduleTests(unittest.TestCase):
    def git(self, *args):
        try:
            return subprocess.check_output(['git', '-C', str(installer_pins.REPO), *args],
                                           stderr=subprocess.DEVNULL).decode()
        except (OSError, subprocess.CalledProcessError):
            self.skipTest('not a Git checkout')

    def test_installer_is_only_the_reviewed_submodule(self):
        name = installer_pins.SUBMODULE
        self.assertEqual(self.git('config', '-f', '.gitmodules', f'submodule.{name}.path').strip(), name)
        self.assertEqual(self.git('config', '-f', '.gitmodules', f'submodule.{name}.url').strip(),
                         'https://github.com/Couch-OS/couch-installer.git')
        self.assertTrue(self.git('ls-files', '--stage', '--', name).startswith('160000 '))
        # Installer source lives in Couch-OS/couch-installer; a tracked copy would drift.
        self.assertEqual(self.git('ls-files', '--', 'tools/installer'), '')


if __name__ == '__main__':
    unittest.main()
