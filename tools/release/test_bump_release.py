from pathlib import Path
import subprocess
import tempfile
import unittest

import bump_release as release

# Dates no release can carry: these fixtures must never look like the
# published tag, which the checker requires no unmanaged file to hold.
OLD = 'v0.1.0-alpha.20200101.1'
NEW = 'v0.1.0-alpha.20200202.2'
README = 'curl -fsSL https://github.com/dangerouslaser/couch/releases/download/%s/install.sh | sh\n'
INSTALLER = ('Release `%s` is published as a prerelease, reported working by a tester on the\n'
             '.128.dev test build.\n'
             '  https://github.com/dangerouslaser/couch/releases/download/%s/install.ps1\n')
class ReleaseLiterals(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / 'docs').mkdir()
        (self.root / 'tools/release').mkdir(parents=True)
        self.write(release.SOURCE, OLD + '\n')
        self.write('README.md', README % OLD)
        self.write('docs/installer.md', INSTALLER % (OLD, OLD))

    def write(self, name, text):
        (self.root / name).write_text(text, encoding='utf-8')

    def read(self, name):
        return (self.root / name).read_text(encoding='utf-8')

    def test_repository_literals_agree_with_the_release_source(self):
        # The regression this tool exists for: a hand-edited tag somewhere.
        self.assertEqual(release.check(), [])

    def test_bump_rewrites_release_commands(self):
        changed = release.bump(NEW, self.root)
        self.assertEqual(changed, [release.SOURCE, 'README.md', 'docs/installer.md'])
        self.assertEqual(self.read(release.SOURCE), NEW + '\n')
        self.assertEqual(self.read('README.md'), README % NEW)
        # A historical build mentioned in prose is not an install command.
        self.assertIn('.128.dev test build', self.read('docs/installer.md'))
        self.assertEqual(release.check(self.root, scan=False), [])
        self.assertEqual(release.bump(NEW, self.root), [])

    def test_check_names_every_file_that_disagrees(self):
        release.bump(NEW, self.root)
        self.write('docs/installer.md', INSTALLER % (NEW, OLD))
        self.write('README.md', 'curl -fsSL https://example.invalid/install.sh | sh\n')
        problems = release.check(self.root, scan=False)
        self.assertEqual(len(problems), 2)
        self.assertIn('README.md: no release tag left', problems[0])
        self.assertTrue(problems[1].startswith('docs/installer.md:3: ' + OLD + ' is not the published ' + NEW),
                        problems[1])

    def test_independent_installer_tag_replaces_legacy_commands(self):
        tag = 'installer-v1.2.3-alpha.1'
        release.bump(tag, self.root)
        self.assertEqual(self.read('README.md'), README % tag)
        self.assertEqual(release.check(self.root, scan=False), [])
        release.bump('installer-v1.2.4', self.root)
        self.assertEqual(self.read('README.md'), README % 'installer-v1.2.4')
        self.assertEqual(release.check(self.root, scan=False), [])
        self.assertEqual(release.bump('installer-v1.2.4', self.root), [])

    def test_repository_cutover_updates_commands_and_retains_the_selected_origin(self):
        release.bump('installer-v1.2.3', self.root, repository='dangerouslaser/couch-installer')
        self.assertEqual(release.source_repository(self.root), 'dangerouslaser/couch-installer')
        self.assertIn('/dangerouslaser/couch-installer/releases/download/installer-v1.2.3/', self.read('README.md'))
        self.assertEqual(release.check(self.root, scan=False), [])
        release.bump('installer-v1.2.4', self.root)
        self.assertIn('/dangerouslaser/couch-installer/releases/download/installer-v1.2.4/', self.read('README.md'))
        self.write('README.md', self.read('README.md').replace('/couch-installer/', '/couch/'))
        self.assertTrue(any('repository differs' in problem for problem in release.check(self.root, scan=False)))

    def test_bad_repository_or_legacy_tag_in_new_repository_does_not_write(self):
        for tag, repository in ((NEW, 'dangerouslaser/couch-installer'), ('installer-v1.0.0', 'someone/unreviewed')):
            with self.assertRaises(ValueError):
                release.bump(tag, self.root, repository=repository)
            self.assertEqual(self.read(release.SOURCE), OLD + '\n')

    def test_a_new_tracked_file_carrying_the_tag_is_reported(self):
        self.assertEqual(release.strays(self.root, OLD), [])  # No work tree, no file list.
        try:
            subprocess.run(['git', 'init', '-q', str(self.root)], check=True, timeout=60)
        except (OSError, subprocess.SubprocessError):
            self.skipTest('git is not available')
        self.write('docs/quickstart.md', README % OLD)
        subprocess.run(['git', '-C', str(self.root), 'add', 'README.md', 'docs', 'tools'],
                       check=True, timeout=60)
        self.assertEqual(release.strays(self.root, OLD), ['docs/quickstart.md'])
        problems = release.check(self.root)
        self.assertEqual(len(problems), 1)
        self.assertTrue(problems[0].startswith('docs/quickstart.md: carries ' + OLD), problems[0])

    def test_invalid_tag_is_rejected_and_nothing_is_written(self):
        for tag in (OLD + '.dev', OLD[1:], 'v0.1.0-alpha.1', 'latest', NEW + '; rm -rf /'):
            with self.assertRaisesRegex(ValueError, 'promotion tag'):
                release.bump(tag, self.root)
        self.assertEqual(self.read('README.md'), README % OLD)
        self.assertEqual(self.read(release.SOURCE), OLD + '\n')
        self.write(release.SOURCE, 'latest\n')
        with self.assertRaisesRegex(ValueError, 'does not hold a release tag'):
            release.check(self.root, scan=False)


if __name__ == '__main__':
    unittest.main()
