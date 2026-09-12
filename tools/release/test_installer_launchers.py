import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import installer_launchers as launchers


class Launchers(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.output = self.root / 'output'
        self.version = 'v0.1.0-alpha.1'
        config = {'schema': 1, 'kind': 'couch-native-installer-release', 'model': 'sanytron-ha100',
                  'version': self.version, 'source_commit': '1' * 40,
                  'payload': {'url': 'https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.1/payload.tar.gz',
                              'size': 100, 'sha256': '2' * 64, 'format': 'tar.gz'}}
        (self.assets / 'installer.json').write_text(json.dumps(config))
        for platform in launchers.PLATFORMS:
            for component in ('host', 'tui'):
                name = f'couch-installer-{component}-{platform}' + ('.exe' if platform.startswith('windows') else '')
                (self.assets / name).write_text('#!/bin/sh\necho NATIVE-FRONTEND\n')

    def test_generated_launchers_pin_all_inputs_and_use_native_backend(self):
        receipt = launchers.generate(self.assets, self.output, self.version)
        self.assertFalse(receipt['published'])
        shell = (self.output / 'install.sh').read_text()
        powershell = (self.output / 'install.ps1').read_text()
        for text in (shell, powershell):
            self.assertIn(receipt['config']['sha256'], text)
            self.assertIn('--native-backend', text)
            self.assertNotIn('python3', text)
            self.assertNotIn('@VERSION@', text)
        if os.name != 'nt':
            subprocess.run(['sh', '-n', str(self.output / 'install.sh')], check=True)
        else:
            checker = self.root / 'check.ps1'
            checker.write_text('param([string]$Target)\n$tokens=$null; $errors=$null; [System.Management.Automation.Language.Parser]::ParseFile($Target,[ref]$tokens,[ref]$errors) | Out-Null; if($errors.Count){throw ($errors | Out-String)}')
            subprocess.run(['powershell', '-NoProfile', '-File', str(checker), '-Target', str(self.output / 'install.ps1')], check=True)

    def test_invalid_version_or_private_configuration_never_generates(self):
        for value in ("v1.2.3';echo bad", '../v1.2.3', 'latest'):
            with self.assertRaisesRegex(ValueError, 'version'):
                launchers.generate(self.assets, self.output, value)
        config = json.loads((self.assets / 'installer.json').read_text())
        config['wifi_password'] = 'must not be published'
        (self.assets / 'installer.json').write_text(json.dumps(config))
        with self.assertRaisesRegex(ValueError, 'configuration'):
            launchers.generate(self.assets, self.output, self.version)
        self.assertFalse(self.output.exists())

    def test_missing_asset_and_existing_output_are_preserved(self):
        path = self.assets / 'couch-installer-host-windows-x64.exe'
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'Missing regular'):
            launchers.generate(self.assets, self.output, self.version)
        self.assertFalse(self.output.exists())
        self.output.mkdir()
        (self.output / 'keep').write_text('previous run')
        with self.assertRaisesRegex(ValueError, 'new launcher'):
            launchers.generate(self.assets, self.output, self.version)
        self.assertEqual((self.output / 'keep').read_text(), 'previous run')

    @unittest.skipIf(os.name == 'nt', 'POSIX controlling-terminal fixture')
    def test_shell_checks_all_downloads_before_starting_native_terminal(self):
        import pty
        launchers.generate(self.assets, self.output, self.version)
        fake = self.root / 'bin'
        fake.mkdir()
        # The only mocked operation is fetching a URL. Real size/SHA checks run.
        curl = fake / 'curl'
        curl.write_text('''#!/bin/sh
while [ "$#" -gt 0 ]; do
    if [ "$1" = --output ]; then shift; destination=$1; fi
    url=$1; shift
done
cp "$FIXTURE_ASSETS/${url##*/}" "$destination"
if [ "${FIXTURE_CORRUPT:-}" = yes ]; then printf x >> "$destination"; fi
''')
        curl.chmod(0o700)
        for corrupt in (False, True):
            pid, terminal = pty.fork()
            if pid == 0:
                env = dict(os.environ, PATH=str(fake) + os.pathsep + os.environ['PATH'],
                           FIXTURE_ASSETS=str(self.assets), FIXTURE_CORRUPT='yes' if corrupt else 'no')
                os.execve('/bin/sh', ['sh', str(self.output / 'install.sh')], env)
            data = bytearray()
            try:
                while True:
                    try:
                        block = os.read(terminal, 4096)
                    except OSError:
                        break
                    if not block:
                        break
                    data.extend(block)
            finally:
                os.close(terminal)
            _, status = os.waitpid(pid, 0)
            self.assertEqual(os.waitstatus_to_exitcode(status), 2 if corrupt else 0)
            self.assertEqual(b'NATIVE-FRONTEND' in data, not corrupt)


if __name__ == '__main__':
    unittest.main()
