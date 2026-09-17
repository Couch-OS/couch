import copy
import json
from pathlib import Path
import tempfile
import unittest

import release_descriptor as descriptor
import installer_launchers as launchers


class IndependentRelease(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.legacy = {'schema': 1, 'kind': 'couch-native-installer-release', 'model': 'sanytron-ha100',
                       'version': 'v0.1.0-alpha.1', 'source_commit': 'a' * 40,
                       'payload': {'url': descriptor.DOWNLOAD + 'v0.1.0-alpha.1/payload.tar.gz',
                                   'size': 100, 'sha256': 'b' * 64, 'format': 'tar.gz'}}
        self.input = self.root / 'os.json'
        self.input.write_text(json.dumps(self.legacy))

    def test_new_installer_keeps_existing_os_bytes_and_identity(self):
        output = self.root / 'installer.json'
        config = descriptor.prepare(self.input, output, 'v1.2.3', 'c' * 40)
        self.assertEqual(config['payload'], self.legacy['payload'])
        self.assertEqual(config['os'], {'version': self.legacy['version'], 'source_commit': 'a' * 40,
                                       'installation_protocol': 1})
        self.assertEqual(config['installer']['source_commit'], 'c' * 40)
        for platform in launchers.PLATFORMS:
            for component in ('host', 'tui'):
                name = f'couch-installer-{component}-{platform}' + ('.exe' if platform.startswith('windows') else '')
                (self.root / name).write_bytes(b'fixture native binary')
        receipt = launchers.generate(self.root, self.root / 'launchers', 'v1.2.3')
        self.assertEqual(receipt['installer'], config['installer'])
        self.assertEqual(receipt['os'], config['os'])
        for name in ('install.sh', 'install.ps1'):
            script = (self.root / 'launchers' / name).read_text()
            self.assertIn(descriptor.DOWNLOAD + 'installer-v1.2.3', script)
            self.assertNotIn(descriptor.DOWNLOAD + self.legacy['version'], script)

    def test_separate_installer_repository_pins_launchers_without_moving_os(self):
        repository = 'dangerouslaser/couch-installer'
        expected = f'https://github.com/{repository}/releases/download/installer-v1.2.3'
        config = descriptor.prepare(self.input, self.root / 'installer.json', 'v1.2.3', 'c' * 40,
                                    installer_repository=repository)
        self.assertEqual(config['installer']['release_url'], expected)
        self.assertEqual(config['payload'], self.legacy['payload'])
        for platform in launchers.PLATFORMS:
            for component in ('host', 'tui'):
                name = f'couch-installer-{component}-{platform}' + ('.exe' if platform.startswith('windows') else '')
                (self.root / name).write_bytes(b'fixture native binary')
        launchers.generate(self.root, self.root / 'launchers', 'v1.2.3')
        for name in ('install.sh', 'install.ps1'):
            self.assertIn(expected, (self.root / 'launchers' / name).read_text())
        explicit = descriptor.prepare(self.input, self.root / 'explicit.json', 'v1.2.3', 'c' * 40,
                                      release_url=expected)
        self.assertEqual(explicit, config)
        with self.assertRaisesRegex(ValueError, 'selected repository'):
            descriptor.prepare(self.input, self.root / 'wrong.json', 'v1.2.3', 'c' * 40,
                               release_url=expected, installer_repository='dangerouslaser/couch')
        with self.assertRaisesRegex(ValueError, 'Unsupported installer repository'):
            descriptor.prepare(self.input, self.root / 'unknown.json', 'v1.2.3', 'c' * 40,
                               installer_repository='other/couch-installer')

    def test_unreviewed_installer_origins_refs_and_paths_are_rejected(self):
        config = descriptor.prepare(self.input, self.root / 'installer.json', 'v1.2.3', 'c' * 40)
        base = 'https://github.com/dangerouslaser/couch-installer/releases/download/installer-v1.2.3'
        for url in (base.replace('github.com', 'example.com'), base.replace('https:', 'http:'),
                    base.replace('dangerouslaser/', 'other/'), base.replace('couch-installer', 'other'),
                    base.replace('installer-v1.2.3', 'latest'), base.replace('installer-v1.2.3', 'installer-v1.2.4'),
                    base + '/', base + '/installer.json', base + '?ref=latest', base + '#fragment'):
            with self.subTest(url=url):
                config['installer']['release_url'] = url
                with self.assertRaises(ValueError):
                    descriptor.validate(config)
        config['installer']['release_url'] = base
        config['payload']['url'] = config['payload']['url'].replace('/couch/', '/couch-installer/')
        with self.assertRaisesRegex(ValueError, 'OS release URL'):
            descriptor.validate(config)

    def test_schema1_normalizes_without_repackaging(self):
        installer, os_release, payload = descriptor.validate(self.legacy)
        self.assertEqual(installer['version'], os_release['version'])
        self.assertEqual(os_release['installation_protocol'], 1)
        self.assertEqual(payload, self.legacy['payload'])

    def test_schema1_retains_legacy_exact_tag_url_admission(self):
        self.legacy['payload']['url'] = descriptor.DOWNLOAD + 'historical-tag/payload.tar.gz'
        self.assertEqual(descriptor.validate(self.legacy)[2], self.legacy['payload'])

    def test_invalid_pin_protocol_and_floating_urls_rejected(self):
        config = descriptor.prepare(self.input, self.root / 'installer.json', 'v1.2.3', 'c' * 40)
        cases = [('os', 'installation_protocol', 2), ('os', 'installation_protocol', True),
                 ('os', 'version', 'v0.2.0'), ('payload', 'sha256', 'bad'), ('payload', 'size', 0),
                 ('payload', 'url', descriptor.DOWNLOAD + 'latest/payload.tar.gz'),
                 ('payload', 'url', self.legacy['payload']['url'] + '?tag=other'),
                 ('installer', 'release_url', descriptor.DOWNLOAD + 'installer-latest'),
                 ('installer', 'release_url', descriptor.DOWNLOAD + 'installer-v9.9.9')]
        for section, key, value in cases:
            with self.subTest(section=section, key=key, value=value):
                bad = copy.deepcopy(config)
                bad[section][key] = value
                with self.assertRaises(ValueError):
                    descriptor.validate(bad)

    def test_rebinding_schema2_preserves_os_and_refuses_overwrite(self):
        first = self.root / 'installer.json'
        config = descriptor.prepare(self.input, first, 'v1.0.0', 'c' * 40)
        second = descriptor.prepare(first, self.root / 'next.json', 'v1.0.1', 'd' * 40)
        self.assertEqual(second['os'], config['os'])
        self.assertEqual(second['payload'], config['payload'])
        with self.assertRaises(FileExistsError):
            descriptor.prepare(self.input, first, 'v1.0.2', 'e' * 40)
        self.assertEqual(json.loads(first.read_text()), config)


if __name__ == '__main__':
    unittest.main()
