import base64
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import prepare


class AdmissionTests(unittest.TestCase):
    def test_build_reference_is_not_an_arbitrary_api_path(self):
        with patch.dict(os.environ, {'BINARY_RUN_ID':'../other', 'CONFIG_SHA256':'1'*64, 'LAUNCHER_SHA256':'2'*64}):
            with self.assertRaisesRegex(ValueError, 'run ID'):
                prepare.prepare(Path('.'), Path('.'), Path('unused'))

    def test_changed_descriptor_is_rejected_before_artifact_access(self):
        with patch.dict(os.environ, {'BINARY_RUN_ID':'123', 'CONFIG_SHA256':'1'*64, 'LAUNCHER_SHA256':'2'*64, 'CONFIG_BASE64':'e30='}), patch.object(prepare.subprocess, 'check_output', side_effect=[b'{"head_sha":"'+prepare.SOURCE.encode()+b'","conclusion":"success","name":"Build installer binaries"}', prepare.SOURCE+'\n']):
            with self.assertRaisesRegex(ValueError, 'descriptor hash'):
                prepare.prepare(Path('absent'), Path('absent'), Path('unused'))

    def test_failed_or_different_source_run_is_never_admitted(self):
        for source, conclusion in [('0'*40,'success'), (prepare.SOURCE,'failure')]:
            data = ('{"head_sha":"'+source+'","conclusion":"'+conclusion+'","name":"Build installer binaries"}').encode()
            with patch.dict(os.environ, {'BINARY_RUN_ID':'123', 'CONFIG_SHA256':'1'*64, 'LAUNCHER_SHA256':'2'*64}), patch.object(prepare.subprocess,'check_output',return_value=data):
                with self.assertRaisesRegex(ValueError, 'source/status'):
                    prepare.prepare(Path('.'), Path('.'), Path('unused'))

    def test_independent_payload_and_host_pins_with_verified_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            downloads = root/'downloads'
            records = {}
            for platform in ('linux-x64', 'macos-x64', 'macos-arm64', 'macos-universal', 'windows-x64'):
                record = {'schema': 1, 'kind': 'couch-installer-native-build',
                          'source_commit': prepare.SOURCE, 'installer_version': prepare.INSTALLER_VERSION,
                          'platform': platform, 'binaries': {}}
                for component in ('host', 'tui'):
                    name = f'couch-installer-{component}'
                    artifact = downloads/f'{name}-{platform}'/(name+('.exe' if platform == 'windows-x64' else ''))
                    artifact.parent.mkdir(parents=True)
                    artifact.write_bytes(f'{platform}-{component}'.encode())
                    record['binaries'][component] = prepare.digest(artifact)
                if platform == 'macos-universal':
                    record.update(kind='couch-installer-universal-build', architectures=['x86_64', 'arm64'],
                                  inputs={name: {'receipt': prepare.digest(downloads/f'couch-installer-build-{name}'/'build.json'),
                                                 'build': records[name]} for name in ('macos-x64', 'macos-arm64')})
                receipt = downloads/f'couch-installer-build-{platform}'/'build.json'
                receipt.parent.mkdir()
                receipt.write_text(json.dumps(record))
                records[platform] = record
            launcher = b'fixture launcher'
            metadata = {'schema': 2, 'kind': 'couch-native-installer-release', 'model': 'sanytron-ha100',
                        'installer': {'version': prepare.INSTALLER_VERSION, 'source_commit': prepare.SOURCE,
                                      'release_url': 'https://github.com/dangerouslaser/couch/releases/download/installer-' + prepare.INSTALLER_VERSION},
                        'os': {'version': prepare.OS_VERSION, 'source_commit': prepare.PAYLOAD_SOURCE,
                               'installation_protocol': 1},
                        'payload': {'url': 'https://example.invalid/payload', 'size': 1, 'sha256': 'a'*64}}
            def invoke(output, installer_source=prepare.SOURCE, payload_source=prepare.PAYLOAD_SOURCE,
                       run_source=prepare.SOURCE,
                       generator_source=prepare.SOURCE, launcher_hash=None, legacy=False,
                       repository='dangerouslaser/couch', config_repository='dangerouslaser/couch'):
                if legacy:
                    config_data = {'schema': 1, 'kind': 'couch-native-installer-release',
                                   'model': 'sanytron-ha100', 'version': prepare.OS_VERSION,
                                   'source_commit': payload_source, 'payload': metadata['payload']}
                else:
                    config_data = json.loads(json.dumps(metadata))
                    config_data['installer']['source_commit'] = installer_source
                    config_data['installer']['release_url'] = f'https://github.com/{config_repository}/releases/download/installer-{prepare.INSTALLER_VERSION}'
                    config_data['os']['source_commit'] = payload_source
                config = json.dumps(config_data).encode()
                env = {'BINARY_RUN_ID': '123', 'CONFIG_BASE64': base64.b64encode(config).decode(),
                       'CONFIG_SHA256': hashlib.sha256(config).hexdigest(),
                       'LAUNCHER_SHA256': launcher_hash or hashlib.sha256(launcher).hexdigest()}
                def generate(*args, **kwargs):
                    expected = root/'frozen'/('tools/release/installer_launchers.py' if legacy
                                              else 'tools/installer/installer_launchers.py')
                    self.assertEqual(Path(args[0][1]), expected)
                    (output/'launchers').mkdir()
                    (output/'launchers/install.ps1').write_bytes(launcher)
                run = json.dumps({'head_sha': run_source, 'conclusion': 'success', 'name': 'Build installer binaries'})
                with patch.dict(os.environ, env), patch.object(prepare, 'INSTALLER_REPOSITORY', repository), patch.object(prepare.subprocess, 'check_output', side_effect=[run, generator_source+'\n']) as external, patch.object(prepare.subprocess, 'run', side_effect=generate):
                    prepare.prepare(downloads, root/'frozen', output)
                    self.assertEqual(external.call_args_list[0].args[0],
                                     ['gh', 'api', f'repos/{repository}/actions/runs/123'])
            self.assertNotEqual(prepare.SOURCE, prepare.PAYLOAD_SOURCE)
            invoke(root/'accepted')
            invoke(root/'separate', repository='dangerouslaser/couch-installer',
                   config_repository='dangerouslaser/couch-installer')
            separate = json.loads((root/'separate/admission.json').read_text())
            self.assertEqual(separate['installer_repository'], 'dangerouslaser/couch-installer')
            admission = json.loads((root/'accepted/admission.json').read_text())
            self.assertEqual(admission['installer'], {'version': prepare.INSTALLER_VERSION, 'source_commit': prepare.SOURCE})
            self.assertEqual(admission['os'], {'version': prepare.OS_VERSION, 'source_commit': prepare.PAYLOAD_SOURCE, 'installation_protocol': 1})
            for name, kwargs, message in (
                ('repository', {'config_repository': 'dangerouslaser/couch-installer'}, 'descriptor installer/OS identity'),
                ('wrong-input-repository', {'repository': 'dangerouslaser/couch-installer'}, 'descriptor installer/OS identity'),
                ('payload', {'payload_source': prepare.SOURCE}, 'descriptor installer/OS identity'),
                ('installer', {'installer_source': prepare.PAYLOAD_SOURCE}, 'descriptor installer/OS identity'),
                ('host', {'run_source': prepare.PAYLOAD_SOURCE}, 'source/status'),
                ('generator', {'generator_source': prepare.PAYLOAD_SOURCE}, 'Launcher source'),
                ('launcher', {'launcher_hash': '0'*64}, 'pinned launcher'),
            ):
                with self.subTest(name=name), self.assertRaisesRegex(ValueError, message):
                    invoke(root/name, **kwargs)
            receipt = downloads/'couch-installer-build-linux-x64/build.json'
            record = json.loads(receipt.read_text())
            record.pop('installer_version')
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'receipt source'):
                invoke(root/'versionless-schema-two')
            record['installer_version'] = prepare.INSTALLER_VERSION
            record['source_commit'] = prepare.PAYLOAD_SOURCE
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'receipt source'):
                invoke(root/'receipt')

            # Historical schema-1 runs predate independent installer versions.
            record['source_commit'] = prepare.SOURCE
            record.pop('installer_version')
            receipt.write_text(json.dumps(record))
            for platform in ('macos-x64', 'macos-arm64', 'windows-x64'):
                path = downloads/f'couch-installer-build-{platform}'/'build.json'
                old = json.loads(path.read_text()); old.pop('installer_version')
                path.write_text(json.dumps(old))
            universal_path = downloads/'couch-installer-build-macos-universal/build.json'
            universal = json.loads(universal_path.read_text()); universal.pop('installer_version')
            for name, item in universal['inputs'].items():
                native = downloads/f'couch-installer-build-{name}'/'build.json'
                item['receipt'] = prepare.digest(native)
                item['build'] = json.loads(native.read_text())
            universal_path.write_text(json.dumps(universal))
            invoke(root/'legacy', legacy=True)
            legacy = json.loads((root/'legacy/admission.json').read_text())
            self.assertEqual(legacy['schema'], 1)
            self.assertEqual(legacy['source_commit'], prepare.SOURCE)
            self.assertEqual(legacy['payload_source_commit'], prepare.PAYLOAD_SOURCE)

    def test_unknown_repository_rejected_before_external_access(self):
        for repository in ('other/couch-installer', 'dangerouslaser/other', '../other', 'https://example.com'):
            with self.subTest(repository=repository), patch.object(prepare, 'INSTALLER_REPOSITORY', repository), patch.object(prepare.subprocess, 'check_output') as external:
                with self.assertRaisesRegex(ValueError, 'installer repository'):
                    prepare.prepare(Path('.'), Path('.'), Path('unused'))
                external.assert_not_called()

    def test_invalid_payload_pin_rejected_before_external_access(self):
        with patch.object(prepare, 'PAYLOAD_SOURCE', '../untrusted'), patch.object(prepare.subprocess, 'check_output') as external:
            with self.assertRaisesRegex(ValueError, 'payload source'):
                prepare.prepare(Path('.'), Path('.'), Path('unused'))
            external.assert_not_called()

    @unittest.skipUnless(os.name == 'nt', 'Windows ConPTY fixture')
    def test_actual_console_cancels_then_closes_completion(self):
        import windows_console
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root/'console.ps1'
            script.write_text('[Console]::WriteLine("Reinstall existing Couch / Cancel"); 1..4 | ForEach-Object { $null = [Console]::ReadKey($true) }; [Console]::WriteLine("SESSION ENDED Enter / Esc close"); $null = [Console]::ReadKey($true); exit 0')
            with patch.dict(os.environ, {'LOCALAPPDATA':str(root/'owner')}):
                result = windows_console.run(script, root/'console.txt')
            self.assertTrue(result['cancel_selected'])
            self.assertTrue(result['completion_closed'])
            self.assertFalse(result['session_created'])

    @unittest.skipUnless(os.name == 'nt' and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted', 'Ephemeral hosted Windows HTTPS fixture')
    def test_https_fixture_and_console_restore_runner_state(self):
        import subprocess
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root/'assets').mkdir(); (root/'launchers').mkdir()
            for name in ('couch-installer-host-windows-x64.exe', 'couch-installer-tui-windows-x64.exe'):
                (root/'assets'/name).write_bytes(b'fixture')
            (root/'assets/installer.json').write_text(json.dumps({
                'schema': 1, 'version': 'v0.1.0-alpha.20260910.24'}))
            script = '''$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Net.Http
$client=[Net.Http.HttpClient]::new()
foreach ($name in @('couch-installer-host-windows-x64.exe','couch-installer-tui-windows-x64.exe','installer.json')) {
 $bytes=$client.GetByteArrayAsync("https://github.com/dangerouslaser/couch/releases/download/v0.1.0-alpha.20260910.24/$name").GetAwaiter().GetResult()
 if ($name -ne 'installer.json' -and [Text.Encoding]::UTF8.GetString($bytes) -ne 'fixture') { throw 'Fixture bytes differ' }
}
$client.Dispose()
[Console]::WriteLine('Reinstall existing Couch / Cancel')
1..4 | ForEach-Object { $null=[Console]::ReadKey($true) }
'''
            (root/'launchers/install.ps1').write_text(script)
            subprocess.run(['powershell','-NoProfile','-ExecutionPolicy','Bypass','-File',str(Path(__file__).with_name('windows_launcher.ps1')),'-Candidate',str(root)],check=True,timeout=160)
            self.assertTrue((root/'result.json').is_file())

    def test_symlinks_and_empty_artifacts_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'empty'; path.write_bytes(b'')
            with self.assertRaisesRegex(ValueError,'size'): prepare.digest(path)
            path.write_bytes(b'fixture')
            self.assertEqual(prepare.digest(path)['size'],7)


if __name__ == '__main__': unittest.main()
