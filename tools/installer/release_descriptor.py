#!/usr/bin/env python3
"""Pin an independent installer release to an existing public OS descriptor.

This creates metadata only: it does not build, download, or publish an OS.
"""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent
DOWNLOAD = 'https://github.com/dangerouslaser/couch/releases/download/'
INSTALLER_REPOSITORIES = ('dangerouslaser/couch', 'Couch-OS/couch-installer')
PROTOCOL = 1
VERSION = r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?'


def require(value, message):
    if not value:
        raise ValueError(message)


def installer_release_url(repository, version):
    require(repository in INSTALLER_REPOSITORIES, 'Unsupported installer repository')
    return f'https://github.com/{repository}/releases/download/installer-{version}'


def identity(value):
    require(isinstance(value, dict) and isinstance(value.get('version'), str)
            and len(value['version']) <= 128 and re.fullmatch(VERSION, value['version'])
            and isinstance(value.get('source_commit'), str)
            and re.fullmatch('[0-9a-f]{40}', value['source_commit']), 'Invalid public release identity')


def validate(config):
    """Return normalized identities while preserving the exact OS payload pin."""
    require(isinstance(config, dict) and type(config.get('schema')) is int
            and config.get('kind') == 'couch-native-installer-release'
            and config.get('model') == 'sanytron-ha100', 'Invalid public installer configuration')
    common = {'schema', 'kind', 'model', 'payload'}
    if config['schema'] == 1:
        require(set(config) == common | {'version', 'source_commit'}, 'Invalid public installer configuration')
        installer = {key: config[key] for key in ('version', 'source_commit')}
        installer['release_url'] = DOWNLOAD + config['version']
        os_release = {key: config[key] for key in ('version', 'source_commit')}
        os_release['installation_protocol'] = PROTOCOL
    elif config['schema'] == 2:
        require(set(config) == common | {'installer', 'os'}, 'Invalid public installer configuration')
        installer, os_release = config['installer'], config['os']
        require(isinstance(installer, dict) and set(installer) == {'version', 'source_commit', 'release_url'}
                and isinstance(os_release, dict) and set(os_release) == {'version', 'source_commit', 'installation_protocol'},
                'Invalid public installer configuration')
        identity(installer)
        require(installer['release_url'] in {installer_release_url(repo, installer['version'])
                                             for repo in INSTALLER_REPOSITORIES},
                'Installer release URL differs from version')
    else:
        raise ValueError('Unsupported installer configuration schema')
    identity(installer)
    identity(os_release)
    require(type(os_release['installation_protocol']) is int and os_release['installation_protocol'] == PROTOCOL,
            'Unsupported installation protocol')
    payload = config['payload']
    url_pattern = (re.escape(DOWNLOAD) + r'[A-Za-z0-9._-]+/[A-Za-z0-9._-]+' if config['schema'] == 1
                   else re.escape(DOWNLOAD + os_release['version'] + '/') + r'[A-Za-z0-9_][A-Za-z0-9._-]*')
    require(isinstance(payload, dict) and set(payload) == {'url', 'size', 'sha256', 'format'}
            and payload['format'] == 'tar.gz' and type(payload['size']) is int
            and 0 < payload['size'] <= 1024**3 and isinstance(payload['sha256'], str)
            and re.fullmatch('[0-9a-f]{64}', payload['sha256'])
            and isinstance(payload['url'], str)
            and re.fullmatch(url_pattern, payload['url']),
            'Invalid public payload pin or OS release URL')
    return installer, os_release, payload


def load(path):
    require(not path.is_symlink() and path.is_file() and 0 < path.stat().st_size <= 65536,
            'Invalid public installer configuration file')
    config = json.loads(path.read_text())
    validate(config)
    return config


def prepare(os_config, output, version, source_commit, release_url=None, installer_repository=None):
    _, os_release, payload = validate(load(os_config))
    if installer_repository is not None:
        expected_url = installer_release_url(installer_repository, version)
        require(release_url is None or release_url == expected_url,
                'Installer release URL differs from selected repository')
    else:
        expected_url = installer_release_url(INSTALLER_REPOSITORIES[0], version)
    config = {'schema': 2, 'kind': 'couch-native-installer-release', 'model': 'sanytron-ha100',
              'installer': {'version': version, 'source_commit': source_commit,
                            'release_url': expected_url if release_url is None else release_url},
              'os': os_release, 'payload': payload}
    validate(config)
    # Exclusive creation preserves existing release metadata and symlinks.
    with output.open('x') as stream:
        stream.write(json.dumps(config, indent=2) + '\n')
    return config


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--os-config', type=Path, required=True, help='Existing schema-1 or schema-2 installer.json selecting the OS')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--version', default=(ROOT / 'VERSION').read_text().strip())
    parser.add_argument('--source-commit', required=True, help='Exact source commit used for installer binaries')
    parser.add_argument('--installer-repository', choices=INSTALLER_REPOSITORIES,
                        help='Reviewed installer repository (default: dangerouslaser/couch)')
    parser.add_argument('--release-url', help='Exact installer-version GitHub release directory in a reviewed repository')
    args = parser.parse_args()
    prepare(args.os_config, args.output, args.version, args.source_commit, args.release_url, args.installer_repository)


if __name__ == '__main__':
    main()
