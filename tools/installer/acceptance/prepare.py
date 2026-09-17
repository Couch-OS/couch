"""Admit frozen build artifacts and one independently pinned public descriptor."""
import base64
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys

SOURCE = os.environ.get('SOURCE_COMMIT', '57a3e22b4e86d8d6620dbaedbf30847e26b9bbd4')
PAYLOAD_SOURCE = os.environ.get('PAYLOAD_SOURCE_COMMIT', '271704728c77c13add1763aea7d1f9bd629c63ca')
INSTALLER_VERSION = os.environ.get('INSTALLER_VERSION', 'v0.1.0')
INSTALLER_REPOSITORY = os.environ.get('INSTALLER_REPOSITORY', 'dangerouslaser/couch')
INSTALLER_REPOSITORIES = ('dangerouslaser/couch', 'Couch-OS/couch-installer')
OS_VERSION = os.environ.get('OS_VERSION', 'v0.1.0-alpha.20260910.24')
# Kept for the frozen fixture helpers which predate separate installer releases.
VERSION = OS_VERSION
PLATFORMS = ('linux-x64', 'macos-universal', 'windows-x64')
VERSION_PATTERN = r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?'


def digest(path):
    if path.is_symlink() or not path.is_file(): raise ValueError('Expected regular artifact')
    data = path.read_bytes()
    if not 0 < len(data) <= 128*1024*1024: raise ValueError('Invalid artifact size')
    return {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


def prepare(downloads, frozen, output):
    if INSTALLER_REPOSITORY not in INSTALLER_REPOSITORIES:
        raise ValueError('Invalid independently pinned installer repository')
    for name, value in (('host', SOURCE), ('payload', PAYLOAD_SOURCE)):
        if not re.fullmatch('[0-9a-f]{40}', value): raise ValueError(f'Invalid independently pinned {name} source')
    for name, value in (('installer', INSTALLER_VERSION), ('OS', OS_VERSION)):
        if not re.fullmatch(VERSION_PATTERN, value): raise ValueError(f'Invalid independently pinned {name} version')
    if not re.fullmatch('[0-9]{1,20}', os.environ['BINARY_RUN_ID']): raise ValueError('Invalid build run ID')
    for name in ('CONFIG_SHA256', 'LAUNCHER_SHA256'):
        if not re.fullmatch('[0-9a-f]{64}', os.environ[name]): raise ValueError('Invalid trusted hash')
    info = json.loads(subprocess.check_output(['gh', 'api', 'repos/'+INSTALLER_REPOSITORY+'/actions/runs/'+os.environ['BINARY_RUN_ID']]))
    if info['head_sha'] != SOURCE or info['conclusion'] != 'success' or info['name'] != 'Build installer binaries':
        raise ValueError('Build run source/status differs')
    if subprocess.check_output(['git', '-C', str(frozen), 'rev-parse', 'HEAD'], text=True).strip() != SOURCE:
        raise ValueError('Launcher source differs')
    config = base64.b64decode(os.environ['CONFIG_BASE64'], validate=True)
    if not 0 < len(config) <= 65536 or hashlib.sha256(config).hexdigest() != os.environ['CONFIG_SHA256']:
        raise ValueError('Public descriptor hash differs')
    metadata = json.loads(config)
    legacy = metadata.get('schema') == 1
    if legacy:
        if (INSTALLER_REPOSITORY != 'dangerouslaser/couch'
                or metadata.get('kind') != 'couch-native-installer-release'
                or metadata.get('version') != OS_VERSION
                or metadata.get('source_commit') != PAYLOAD_SOURCE):
            raise ValueError('Legacy public descriptor OS identity differs')
        launcher_version = OS_VERSION
    elif (metadata.get('schema') != 2 or metadata.get('kind') != 'couch-native-installer-release'
            or metadata.get('installer') != {
                'version': INSTALLER_VERSION,
                'source_commit': SOURCE,
                'release_url': f'https://github.com/{INSTALLER_REPOSITORY}/releases/download/installer-{INSTALLER_VERSION}',
            }
            or metadata.get('os') != {
                'version': OS_VERSION,
                'source_commit': PAYLOAD_SOURCE,
                'installation_protocol': 1,
            }):
        raise ValueError('Public descriptor installer/OS identity differs')
    else:
        launcher_version = INSTALLER_VERSION
    output.mkdir(mode=0o700)
    assets = output/'assets'; assets.mkdir()
    (assets/'installer.json').write_bytes(config)
    records = {}
    for platform in PLATFORMS:
        receipt_path = downloads/f'couch-installer-build-{platform}'/'build.json'
        if receipt_path.stat().st_size > 1024*1024: raise ValueError('Receipt too large')
        receipt = json.loads(receipt_path.read_bytes())
        expected_kind = 'couch-installer-universal-build' if platform == 'macos-universal' else 'couch-installer-native-build'
        expected_version = receipt.get('installer_version') if legacy else INSTALLER_VERSION
        if ((receipt.get('schema'), receipt.get('kind'), receipt.get('source_commit'),
                receipt.get('installer_version'), receipt.get('platform')) != (
                    1, expected_kind, SOURCE, expected_version, platform)
                or legacy and expected_version not in (None, OS_VERSION)):
            raise ValueError('Build receipt source/version/platform differs')
        if platform == 'macos-universal':
            if receipt['architectures'] != ['x86_64', 'arm64']: raise ValueError('Universal architectures differ')
            if set(receipt['inputs']) != {'macos-x64', 'macos-arm64'}: raise ValueError('Missing native universal inputs')
            for name, item in receipt['inputs'].items():
                native = downloads/f'couch-installer-build-{name}'/'build.json'
                if digest(native) != item['receipt'] or json.loads(native.read_bytes()) != item['build']:
                    raise ValueError('Universal input receipt differs')
                nested_version = item['build'].get('installer_version')
                if (item['build'].get('source_commit') != SOURCE
                        or legacy and nested_version not in (None, OS_VERSION)
                        or not legacy and nested_version != INSTALLER_VERSION):
                    raise ValueError('Mixed source commits or installer versions')
        extension = '.exe' if platform == 'windows-x64' else ''
        for component in ('host', 'tui'):
            name = f'couch-installer-{component}'
            original = downloads/f'{name}-{platform}'/(name+extension)
            if digest(original) != receipt['binaries'][component]: raise ValueError('Binary hash differs')
            shutil.copyfile(original, assets/f'{name}-{platform}{extension}')
        records[platform] = receipt
    generator = frozen/'tools/installer/installer_launchers.py'
    if legacy and not generator.is_file():
        generator = frozen/'tools/release/installer_launchers.py'
    subprocess.run([sys.executable, str(generator), '--assets', str(assets),
                    '--output', str(output/'launchers'), '--version', launcher_version], check=True, timeout=30)
    launcher = output/'launchers/install.ps1'
    if digest(launcher)['sha256'] != os.environ['LAUNCHER_SHA256']:
        raise ValueError('Generated final launcher differs from independently pinned launcher')
    identities = ({'schema': 1, 'source_commit': SOURCE, 'payload_source_commit': PAYLOAD_SOURCE}
                  if legacy else {'schema': 2,
                      'installer': {'version': INSTALLER_VERSION, 'source_commit': SOURCE},
                      'os': {'version': OS_VERSION, 'source_commit': PAYLOAD_SOURCE, 'installation_protocol': 1}})
    (output/'admission.json').write_text(json.dumps({**identities,
        'installer_repository':INSTALLER_REPOSITORY, 'binary_run_id':int(os.environ['BINARY_RUN_ID']), 'config':digest(assets/'installer.json'),
        'payload':metadata['payload'], 'launcher':digest(launcher), 'builds':records}, indent=2)+'\n')


if __name__ == '__main__':
    prepare(*(Path(p).resolve() for p in sys.argv[1:]))
