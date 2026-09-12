#!/usr/bin/env python3
"""Generate immutable native launchers from the exact flat release assets."""
import argparse
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = ('linux-x64', 'macos-universal', 'windows-x64')


def require(value, message):
    if not value:
        raise ValueError(message)


def asset(directory, name, limit=128 * 1024 * 1024):
    path = directory / name
    require(not path.is_symlink() and path.is_file(), 'Missing regular release asset: ' + name)
    size = path.stat().st_size
    require(0 < size <= limit, 'Invalid release asset size: ' + name)
    digest = hashlib.sha256()
    with path.open('rb') as source:
        count = 0
        while block := source.read(1024 * 1024):
            count += len(block)
            require(count <= size, 'Release asset changed during hashing')
            digest.update(block)
    require(count == size, 'Release asset changed during hashing')
    return {'file': name, 'size': size, 'sha256': digest.hexdigest()}


def generate(directory, output, version):
    require(re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?', version), 'Invalid release version')
    require(not output.exists() and not output.is_symlink(), 'Use a new launcher output directory')
    config_asset = asset(directory, 'installer.json', 1024 * 1024)
    config = json.loads((directory / 'installer.json').read_text())
    require(set(config) == {'schema', 'kind', 'model', 'version', 'payload', 'source_commit'}
            and config['schema'] == 1 and config['kind'] == 'couch-native-installer-release'
            and config['model'] == 'sanytron-ha100' and config['version'] == version
            and re.fullmatch('[0-9a-f]{40}', config['source_commit']), 'Invalid public installer configuration')
    payload = config['payload']
    require(set(payload) == {'url', 'size', 'sha256', 'format'} and payload['format'] == 'tar.gz'
            and isinstance(payload['size'], int) and 0 < payload['size'] <= 1024 * 1024 * 1024
            and re.fullmatch('[0-9a-f]{64}', payload['sha256'])
            and re.fullmatch(r'https://github\.com/dangerouslaser/couch/releases/download/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+', payload['url']),
            'Invalid public payload pin')
    platforms = {}
    for platform in PLATFORMS:
        extension = '.exe' if platform.startswith('windows') else ''
        platforms[platform] = {component: asset(directory, f'couch-installer-{component}-{platform}{extension}')
                               for component in ('host', 'tui')}
    substitutions = {'VERSION': version, 'CONFIG_SIZE': str(config_asset['size']), 'CONFIG_HASH': config_asset['sha256']}
    cases = []
    for platform, selector in [('linux-x64', 'Linux:x86_64'), ('macos-universal', 'Darwin:x86_64|Darwin:arm64')]:
        assignments = []
        for component, pin in platforms[platform].items():
            assignments.extend([f"{component}_file='{pin['file']}'", f"{component}_size='{pin['size']}'", f"{component}_hash='{pin['sha256']}'"])
        cases.append('    ' + selector + ') ' + '; '.join(assignments) + ' ;;')
    substitutions['PLATFORMS'] = '\n'.join(cases)
    for component, pin in platforms['windows-x64'].items():
        for key, value in [('FILE', pin['file']), ('SIZE', pin['size']), ('HASH', pin['sha256'])]:
            substitutions[component.upper() + '_' + key] = str(value)
    rendered = {}
    for name, template in [('install.sh', 'bootstrap-native.sh.in'), ('install.ps1', 'bootstrap-native.ps1.in')]:
        text = (ROOT / 'tools/installer' / template).read_text()
        for key, value in substitutions.items():
            text = text.replace('@' + key + '@', value)
        require(not re.search(r'@[A-Z_]+@', text), 'Unresolved launcher pin')
        rendered[name] = text
    output.mkdir(parents=True, mode=0o700)
    for name, text in rendered.items():
        (output / name).write_text(text, newline='\n')
    receipt = {'schema': 1, 'version': version, 'source_commit': config['source_commit'],
               'assets': platforms, 'config': config_asset,
               'launchers': {name: asset(output, name) for name in rendered}, 'published': False}
    (output / 'launchers.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    generate(args.assets, args.output, args.version)
