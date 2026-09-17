#!/usr/bin/env python3
"""Collect and verify installer-only corresponding source from an exact Git commit.

This tool is self-contained inside ``tools/installer``. It deliberately cannot
produce a full Couch OS corresponding-source archive.
"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib


MAX_FILE = 1024 * 1024 * 1024
MANIFESTS = ('tools/installer/tui/Cargo.toml',
             'tools/installer/host/Cargo.toml',
             'tools/installer/linux_stage/probe/Cargo.toml',
             'tools/installer/linux_stage/storage/Cargo.toml')
PROJECT_FILES = {'COPYING', 'LICENSE', 'README.md', '.gitignore',
                 '.github/workflows/installer-binaries.yml',
                 '.github/workflows/installer-windows-launcher-acceptance.yml'}
EXCLUDE_PARTS = {'target', '__pycache__', '.git'}
FORBIDDEN_SUFFIXES = {'.img', '.apk', '.so', '.a', '.o', '.pem', '.key', '.elf', '.bin'}


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def checked_path(name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or '\\' in name
            or any(part in ('', '.', '..') for part in name.split('/'))
            or any(ord(character) < 32 for character in name)):
        raise ValueError('Unsafe source path')
    return path


def write(path, data):
    path = Path(path)
    if path.is_symlink():
        raise ValueError('Symlink output refused')
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        if path.read_bytes() != data:
            raise ValueError('Existing source differs: ' + path.name)
        return
    temporary = path.with_name(path.name + '.partial')
    with temporary.open('xb') as stream:
        stream.write(data)
    temporary.replace(path)


def report(path, value):
    path = Path(path)
    if path.is_symlink():
        raise ValueError('Symlink report output refused')
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.report-new')
    with temporary.open('xb') as stream:
        stream.write((json.dumps(value, indent=2, sort_keys=True) + '\n').encode())
    temporary.replace(path)


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.PIPE)


def commit_id(repo, revision):
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('A full Git commit is required')
    commit = git(repo, 'rev-parse', '--verify', revision + '^{commit}').decode().strip()
    if commit != revision:
        raise ValueError('A full Git commit is required')
    return commit


def allowed(name):
    path = checked_path(name)
    if name not in PROJECT_FILES and not name.startswith('tools/installer/'):
        return False
    if any(part in EXCLUDE_PARTS for part in path.parts):
        return False
    if path.suffix.lower() in FORBIDDEN_SUFFIXES:
        raise ValueError('Binary/private input in installer source: ' + name)
    if path.name in ('.env', 'local.env', 'id_rsa', 'id_ed25519'):
        raise ValueError('Private input in installer source')
    return True


def tree_hashes(root):
    root = Path(root)
    values = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('Source archive cannot contain symlinks')
        if path.is_file():
            values[path.relative_to(root).as_posix()] = sha(path)
        elif not path.is_dir():
            raise ValueError('Special file in sources')
    return values


def project(repo, revision, output):
    repo, output = Path(repo), Path(output)
    commit = commit_id(repo, revision)
    files, excluded = {}, []
    for entry in filter(None, git(repo, 'ls-tree', '-rz', '--full-tree', commit).split(b'\0')):
        header, raw_name = entry.split(b'\t', 1)
        mode, kind, object_id = header.decode().split()
        name = raw_name.decode('utf-8')
        if not allowed(name):
            excluded.append(name)
            continue
        if kind != 'blob' or mode not in ('100644', '100755'):
            raise ValueError('Source symlinks/submodules require explicit review')
        data = git(repo, 'cat-file', 'blob', object_id)
        if data.startswith((b'\x7fELF', b'MZ')):
            raise ValueError('Executable binary in installer source: ' + name)
        path = output / 'couch' / name
        write(path, data)
        path.chmod(0o755 if mode == '100755' else 0o644)
        files[name] = hashlib.sha256(data).hexdigest()
    required = (*MANIFESTS, '.github/workflows/installer-binaries.yml',
                '.github/workflows/installer-windows-launcher-acceptance.yml')
    missing = [name for name in required if name not in files]
    if missing:
        raise ValueError('Missing installer source/build inputs: ' + ', '.join(missing))
    if not ({'COPYING', 'LICENSE'} & files.keys()):
        raise ValueError('Missing installer license text')
    manifest = {'schema': 1, 'kind': 'couch-project-source', 'scope': 'installer',
                'complete': True, 'commit': commit,
                'source_date_epoch': int(git(repo, 'show', '-s', '--format=%ct', commit)),
                'files': files, 'excluded_outside_installer_scope': excluded}
    report(output / 'project.json', manifest)
    return manifest


def require_scope(path, kind):
    value = json.loads(Path(path).read_text())
    if (value.get('schema') != 1 or value.get('kind') != kind
            or value.get('scope') != 'installer' or value.get('complete') is not True):
        raise ValueError('Invalid or non-installer source component: ' + Path(path).name)
    return value


def cargo_sources(output, offline=False):
    output = Path(output); root = output / 'couch'
    require_scope(output / 'project.json', 'couch-project-source')
    absent = [name for name in MANIFESTS if not (root / name).is_file()]
    if absent:
        raise ValueError('Missing installer Cargo manifests: ' + ', '.join(absent))
    locks = [str(PurePosixPath(name).parent / 'Cargo.lock') for name in MANIFESTS]
    absent = [name for name in locks if not (root / name).is_file()]
    if absent:
        raise ValueError('Missing installer Cargo locks: ' + ', '.join(absent))
    command = ['cargo', 'vendor', '--locked', '--versioned-dirs',
               '--manifest-path', str(root / MANIFESTS[0])]
    if offline:
        command.append('--offline')
    for name in MANIFESTS[1:]:
        command.extend(['--sync', str(root / name)])
    command.append(str(output / 'cargo-vendor'))
    config = subprocess.check_output(command, cwd=root).decode()
    config = config.replace(str(output / 'cargo-vendor'), 'cargo-vendor')
    write(output / 'cargo-config/vendor.toml', config.encode())
    packages = []
    for directory in sorted((output / 'cargo-vendor').iterdir()):
        if not directory.is_dir() or directory.is_symlink():
            raise ValueError('Unexpected vendor directory')
        metadata = tomllib.loads((directory / 'Cargo.toml').read_text())['package']
        notices = [path.relative_to(directory).as_posix() for path in directory.rglob('*')
                   if path.is_file() and any(word in path.name.lower()
                                             for word in ('license', 'licence', 'copying', 'notice', 'copyright'))]
        packages.append({'name': metadata['name'], 'version': metadata['version'],
                         'license': metadata.get('license'),
                         'license_file': metadata.get('license-file'),
                         'directory': directory.name, 'notice_files': sorted(notices)})
    present = {(package['name'], package['version']) for package in packages}
    for lock in locks:
        for package in tomllib.loads((root / lock).read_text())['package']:
            if package.get('source') and (package['name'], package['version']) not in present:
                raise ValueError('A locked Cargo source is absent')
    for name in MANIFESTS:
        subprocess.run(['cargo', 'metadata', '--format-version=1', '--locked', '--offline',
                        '--all-features', '--config', str(output / 'cargo-config/vendor.toml'),
                        '--manifest-path', str(root / name)], cwd=root,
                       stdout=subprocess.DEVNULL, check=True)
    result = {'schema': 1, 'kind': 'couch-cargo-sources', 'scope': 'installer',
              'complete': True, 'manifests': list(MANIFESTS), 'locks': locks,
              'packages': packages, 'files': tree_hashes(output / 'cargo-vendor'),
              'config_sha256': sha(output / 'cargo-config/vendor.toml')}
    report(output / 'cargo.json', result)
    return result


def complete_mit_grant(data):
    normalized = b' '.join(data.lower().split())
    return (b'permission is hereby granted, free of charge' in normalized
            and b'the software is provided' in normalized)


def license_supplement(output, directory, manifest_path):
    manifest = json.loads(Path(manifest_path).read_text())
    if manifest.get('schema') != 1 or manifest.get('kind') != 'couch-reviewed-license-supplements':
        raise ValueError('Invalid license supplement review')
    review = manifest['packages'].get(directory.name)
    if review is None:
        return None
    metadata = tomllib.loads((directory / 'Cargo.toml').read_text())['package']
    commit = json.loads((directory / '.cargo_vcs_info.json').read_text())['git']['sha1']
    if (sha(directory / 'Cargo.toml') != review['cargo_toml_sha256']
            or commit != review['published_git_commit']
            or metadata.get('license') != review['declared_license']):
        raise ValueError('License supplement differs from published package identity')
    if (review['declared_license'] not in ('MIT', 'MIT OR Apache-2.0')
            or not review.get('reason') or not review.get('files')):
        raise ValueError('License supplement needs a reviewed MIT selection and explanation')
    notices = []
    for filename in review['files']:
        checked_path(filename)
        provenance = manifest['files'][filename]
        path = Path(manifest_path).parent / filename
        if path.is_symlink() or not path.is_file() or sha(path) != provenance['sha256']:
            raise ValueError('License supplement text checksum differs')
        data = path.read_bytes()
        if len(data) > 2 * 1024 * 1024 or b'\0' in data or not complete_mit_grant(data):
            raise ValueError('License supplement lacks the reviewed standard MIT terms')
        if not provenance['url'].startswith('https://'):
            raise ValueError('License supplement requires explicit public HTTPS provenance')
        relative = directory.name + '/reviewed-supplement/' + filename
        write(output / 'cargo-notices' / relative, data)
        notices.append({'file': relative, **provenance,
                        'source': 'standard license terms; not an upstream copyright notice'})
    relative = directory.name + '/reviewed-supplement/Cargo.toml'
    write(output / 'cargo-notices' / relative, (directory / 'Cargo.toml').read_bytes())
    notices.append({'file': relative, 'sha256': sha(directory / 'Cargo.toml'),
                    'source': 'unchanged published crate metadata'})
    return {'package': directory.name, 'commit': commit, 'notices': notices,
            'reviewed_supplement': review, 'upstream_notice_recovered': False,
            'attribution': 'Retained unchanged in published crate sources; no holder or year inferred.'}


def cargo_notices(output, cache, offline=False, overrides=None, supplements=None):
    output, cache = Path(output), Path(cache)
    if supplements is None:
        default_supplements = Path(__file__).with_name('cargo-license-supplements.json')
        supplements = default_supplements if default_supplements.is_file() else None
    inventory = require_scope(output / 'cargo.json', 'couch-cargo-sources')
    collected, errors = [], []
    for package in inventory['packages']:
        if package['notice_files']:
            continue
        directory = output / 'cargo-vendor' / package['directory']
        try:
            supplement = license_supplement(output, directory, supplements) if supplements else None
            if supplement is not None:
                collected.append(supplement)
                continue
            metadata = tomllib.loads((directory / 'Cargo.toml').read_text())['package']
            commit = json.loads((directory / '.cargo_vcs_info.json').read_text())['git']['sha1']
            if not re.fullmatch('[0-9a-f]{40}', commit):
                raise ValueError('Missing published Git source identity')
            repository = (overrides or {}).get(package['directory'],
                                                       metadata.get('repository') or metadata.get('homepage', ''))
            match = re.match(r'https?://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)', repository)
            if not match:
                raise ValueError('Provide a reviewed GitHub repository override for this crate')
            owner, name = match.groups(); name = name.removesuffix('.git')
            url = f'https://github.com/{owner}/{name}.git'
            repository_cache = cache / (owner + '--' + name + '.git')
            if not repository_cache.exists():
                if offline:
                    raise ValueError('Notice repository absent from offline cache')
                repository_cache.mkdir(parents=True)
                subprocess.run(['git', 'init', '--bare', str(repository_cache)], check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                git(repository_cache, 'remote', 'add', 'origin', url)
                git(repository_cache, 'config', 'remote.origin.promisor', 'true')
                git(repository_cache, 'config', 'remote.origin.partialclonefilter', 'blob:none')
            try:
                git(repository_cache, '-c', 'remote.origin.promisor=false',
                    'cat-file', '-e', commit + '^{commit}')
            except subprocess.CalledProcessError:
                if offline:
                    raise ValueError('Published commit absent from offline notice cache')
                git(repository_cache, 'fetch', '--filter=blob:none', '--depth=1', 'origin', commit)
            notices = []
            for entry in filter(None, git(repository_cache, 'ls-tree', '-rz', commit).split(b'\0')):
                header, raw_name = entry.split(b'\t', 1)
                mode, kind, object_id = header.decode().split()
                filename = raw_name.decode(); lower = filename.lower()
                if (not any(word in lower for word in ('license', 'licence', 'copying', 'copyright', 'notice'))
                        and PurePosixPath(filename).name.upper() != 'AUTHORS'):
                    continue
                if kind != 'blob' or mode not in ('100644', '100755'):
                    continue
                checked_path(filename)
                data = subprocess.check_output(
                    ['git', '-c', 'remote.origin.promisor=false', '-C', str(repository_cache),
                     'cat-file', 'blob', object_id], stderr=subprocess.PIPE) if offline else git(
                         repository_cache, 'cat-file', 'blob', object_id)
                if len(data) > 2 * 1024 * 1024 or b'\0' in data:
                    continue
                if PurePosixPath(filename).name.upper() == 'AUTHORS' and not complete_mit_grant(data):
                    continue
                relative = package['directory'] + '/' + filename
                write(output / 'cargo-notices' / relative, data)
                notices.append({'file': relative, 'sha256': hashlib.sha256(data).hexdigest(),
                                'url': f'https://github.com/{owner}/{name}/blob/{commit}/{filename}'})
            if not notices:
                for path in directory.rglob('*.rs'):
                    data = path.read_bytes()
                    if complete_mit_grant(data):
                        relative = package['directory'] + '/source-header/' + path.relative_to(directory).as_posix()
                        write(output / 'cargo-notices' / relative, data)
                        notices.append({'file': relative, 'sha256': hashlib.sha256(data).hexdigest(),
                                        'source': 'unchanged published crate source header'})
                        break
            if not notices:
                raise ValueError('No upstream notice text found; manual review required')
            collected.append({'package': package['directory'], 'repository': url,
                              'commit': commit, 'notices': notices})
        except (KeyError, ValueError, OSError, subprocess.SubprocessError) as error:
            errors.append({'package': package['directory'], 'error': str(error)})
    result = {'schema': 1, 'kind': 'couch-cargo-notices', 'scope': 'installer',
              'complete': not errors, 'packages': collected, 'errors': errors,
              'files': tree_hashes(output / 'cargo-notices')}
    report(output / 'cargo-notices.json', result)
    if errors:
        raise ValueError(f'{len(errors)} Cargo notice sets remain incomplete')
    return result


def validate_rust_receipt(receipt):
    if (receipt.get('schema') != 1 or receipt.get('kind') != 'couch-external-source'
            or receipt.get('component') != 'rust-stdlib'):
        raise ValueError('Installer source accepts only a Rust standard-library source receipt')
    files = receipt.get('files', {})
    required = [receipt.get('source_archive'), receipt.get('configuration'),
                receipt.get('build_recipe'), receipt.get('toolchain_receipt')]
    if not files or any(not name or name not in files for name in required):
        raise ValueError('Rust source receipt needs source, configuration, build recipe and toolchain bytes')
    if not re.fullmatch('[0-9a-f]{64}', receipt.get('binary_sha256', '')):
        raise ValueError('Rust source must identify its corresponding standard-library binary')
    if (not isinstance(receipt.get('rust_release'), str) or not receipt['rust_release']
            or len(receipt['rust_release']) > 128
            or not re.fullmatch('[0-9a-f]{40}', receipt.get('rust_commit', ''))):
        raise ValueError('Rust source receipt must identify the exact compiler release and commit')
    return files


def external_rust_sources(directory, receipt_file, output):
    """Import the audited Rust source/toolchain component used by native binaries."""
    directory, output = Path(directory), Path(output)
    receipt = json.loads(Path(receipt_file).read_text())
    files = validate_rust_receipt(receipt)
    for filename, expected in files.items():
        checked_path(filename)
        path = directory / filename
        if path.is_symlink() or not path.is_file() or sha(path) != expected:
            raise ValueError('Rust source receipt checksum differs')
        if path.suffix.lower() in FORBIDDEN_SUFFIXES or path.read_bytes()[:4] == b'\x7fELF':
            raise ValueError('Binary artifact supplied instead of Rust corresponding source')
        write(output / 'external/rust-stdlib' / filename, path.read_bytes())
    result = {**receipt, 'scope': 'installer', 'complete': True}
    report(output / 'rust-stdlib.json', result)
    return result


def assemble(output, archive_path):
    output, archive_path = Path(output), Path(archive_path)
    included, components = {}, {}
    for name, directory, kind in (
            ('project', 'couch', 'couch-project-source'),
            ('cargo', 'cargo-vendor', 'couch-cargo-sources'),
            ('cargo-notices', 'cargo-notices', 'couch-cargo-notices'),
            ('rust-stdlib', 'external/rust-stdlib', 'couch-external-source')):
        receipt = output / (name + '.json')
        if not receipt.is_file():
            raise ValueError('Missing installer source component: ' + name)
        value = require_scope(receipt, kind)
        if name == 'rust-stdlib':
            validate_rust_receipt(value)
        if value.get('errors'):
            raise ValueError('Incomplete installer source component: ' + name)
        if tree_hashes(output / directory) != value['files']:
            raise ValueError('Source files changed after collection: ' + name)
        components[name] = value; included[name + '.json'] = sha(receipt)
        for path, checksum in value['files'].items():
            checked_path(path); included[directory + '/' + path] = checksum
    expected = {package['directory'] for package in components['cargo']['packages']
                if not package['notice_files']}
    if {package['package'] for package in components['cargo-notices']['packages']} != expected:
        raise ValueError('Cargo notice coverage differs from locked packages')
    expected_locks = [str(PurePosixPath(name).parent / 'Cargo.lock') for name in MANIFESTS]
    if (components['cargo'].get('manifests') != list(MANIFESTS)
            or components['cargo'].get('locks') != expected_locks):
        raise ValueError('Installer Cargo workspace inventory differs')
    for name in (*MANIFESTS, *expected_locks):
        if name not in components['project']['files']:
            raise ValueError('Installer project source omits a Cargo manifest or lock: ' + name)
    config = output / 'cargo-config/vendor.toml'
    if sha(config) != components['cargo']['config_sha256']:
        raise ValueError('Cargo source replacement configuration changed')
    included['cargo-config/vendor.toml'] = sha(config)
    notices = ['# Couch installer source and third-party notices', '',
               'Couch uses GPL-3.0-or-later (see couch/COPYING).',
               'Complete vendored dependency sources retain their published notices.', '',
               '## Rust dependencies', '']
    for package in components['cargo']['packages']:
        notices.append(f"- {package['name']} {package['version']}: {package.get('license') or 'see license-file'}; source cargo-vendor/{package['directory']}; notices: {', '.join(package['notice_files']) or 'see cargo-notices'}")
    notices.extend(['', '## Scope', '',
                    'This archive covers installer source, its four locked Cargo workspaces, dependency notices, and one audited Rust standard-library source/toolchain component.',
                    'The selected OS payload has separate corresponding source. Kernel, APK, BusyBox, BlueZ and owner-local vendor inputs are not covered here.', ''])
    write(output / 'NOTICES.md', '\n'.join(notices).encode())
    included['NOTICES.md'] = sha(output / 'NOTICES.md')
    manifest = {'schema': 1, 'kind': 'couch-installer-corresponding-source-archive',
                'scope': 'installer', 'complete': True,
                'project_commit': components['project']['commit'], 'files': included,
                'os_source_covered': False, 'rust_source_component_included': True,
                'native_platform_toolchains_verified': False}
    report(output / 'SOURCE-MANIFEST.json', manifest)
    names = sorted([*included, 'SOURCE-MANIFEST.json'])
    if archive_path.exists():
        raise ValueError('Refusing to overwrite a source archive')
    archive_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = archive_path.with_name(archive_path.name + '.partial')
    try:
        with temporary.open('xb') as stream, gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as compressed, tarfile.open(fileobj=compressed, mode='w|') as archive:
            for name in names:
                path = output / name; entry = tarfile.TarInfo('couch-installer-source/' + name)
                entry.size = path.stat().st_size; entry.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
                entry.mtime = components['project']['source_date_epoch']
                with path.open('rb') as source:
                    archive.addfile(entry, source)
        temporary.replace(archive_path)
    finally:
        temporary.unlink(missing_ok=True)
    result = {'archive': archive_path.name, 'sha256': sha(archive_path),
              'project_commit': components['project']['commit'], 'scope': 'installer',
              'complete': True, 'os_source_covered': False,
              'rust_source_component_included': True,
              'native_platform_toolchains_verified': False}
    report(archive_path.with_name(archive_path.name + '.json'), result)
    return result


def verify_archive(path):
    hashes, manifest = {}, None
    with gzip.open(path, 'rb') as compressed, tarfile.open(fileobj=compressed, mode='r|') as archive:
        for count, entry in enumerate(archive):
            if count >= 200000 or not entry.isfile() or entry.size > MAX_FILE:
                raise ValueError('Unexpected installer source archive member')
            prefix = 'couch-installer-source/'
            if not entry.name.startswith(prefix):
                raise ValueError('Unexpected installer source archive prefix')
            name = entry.name[len(prefix):]; checked_path(name)
            if name in hashes or name == 'SOURCE-MANIFEST.json' and manifest is not None:
                raise ValueError('Duplicate installer source archive member')
            stream = archive.extractfile(entry)
            if name == 'SOURCE-MANIFEST.json':
                if entry.size > 16 * 1024 * 1024:
                    raise ValueError('Oversized installer source manifest')
                manifest = json.loads(stream.read())
            else:
                digest = hashlib.sha256()
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(block)
                hashes[name] = digest.hexdigest()
    if (not manifest or manifest.get('schema') != 1
            or manifest.get('kind') != 'couch-installer-corresponding-source-archive'
            or manifest.get('scope') != 'installer' or manifest.get('complete') is not True
            or manifest.get('os_source_covered') is not False
            or manifest.get('rust_source_component_included') is not True
            or manifest.get('native_platform_toolchains_verified') is not False
            or hashes != manifest.get('files')):
        raise ValueError('Installer source archive differs from its complete scoped manifest')
    return {'archive': Path(path).name, 'sha256': sha(path),
            'project_commit': manifest['project_commit'], 'scope': 'installer',
            'verified_files': len(hashes), 'os_source_covered': False,
            'rust_source_component_included': True,
            'native_platform_toolchains_verified': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    command = sub.add_parser('project'); command.add_argument('--repo', type=Path, required=True); command.add_argument('--commit', required=True); command.add_argument('--output', type=Path, required=True)
    command = sub.add_parser('cargo'); command.add_argument('--output', type=Path, required=True); command.add_argument('--offline', action='store_true')
    command = sub.add_parser('cargo-notices'); command.add_argument('--output', type=Path, required=True); command.add_argument('--cache', type=Path, required=True); command.add_argument('--offline', action='store_true'); command.add_argument('--repository-overrides', type=Path); command.add_argument('--supplements', type=Path)
    command = sub.add_parser('external-rust'); command.add_argument('--directory', type=Path, required=True); command.add_argument('--receipt', type=Path, required=True); command.add_argument('--output', type=Path, required=True)
    command = sub.add_parser('assemble'); command.add_argument('--output', type=Path, required=True); command.add_argument('--archive', type=Path, required=True)
    command = sub.add_parser('verify-archive'); command.add_argument('--archive', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'project': project(args.repo, args.commit, args.output)
    elif args.command == 'cargo': cargo_sources(args.output, args.offline)
    elif args.command == 'cargo-notices': cargo_notices(args.output, args.cache, args.offline, json.loads(args.repository_overrides.read_text()) if args.repository_overrides else None, args.supplements)
    elif args.command == 'external-rust': external_rust_sources(args.directory, args.receipt, args.output)
    elif args.command == 'assemble': print(json.dumps(assemble(args.output, args.archive)))
    elif args.command == 'verify-archive': print(json.dumps(verify_archive(args.archive)))


if __name__ == '__main__':
    main()
