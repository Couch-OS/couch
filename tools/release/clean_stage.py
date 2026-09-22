#!/usr/bin/env python3
"""Build a deterministic, noninstallable staging archive from explicit clean inputs.

Never reads a running device or extracts an archive onto the host filesystem.
The manifest hashes identify inputs; they do not authenticate their publisher.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import tarfile

REPO = Path(__file__).resolve().parents[2]
BINARIES = frozenset(('couch-gui', 'couch-confd', 'couch-system', 'couch-kodi', 'couch-webos', 'couch-tizen',
    'couch-coreelec', 'couch-bt-bridge', 'couch-bt-hid', 'couch-sonos', 'couch-hue', 'couch-ha', 'couch-ir', 'couch-voice', 'couch-bluetoothd', 'fbcon'))
LICENSE_FILES = frozenset(('Lato-OFL.txt', 'Inter-OFL.txt', 'Lucide-ISC.txt', 'IRDB-MIT.txt', 'IRDB-CC0.txt', 'BlueZ-GPL-2.0.txt'))
RECOVERY_CGI = frozenset(('save', 'setpw', 'scan', 'enroll'))
GENERATED = {
    'opt/couch/config.json': b'{"schema_version":1}\n',
    'etc/hostname': b'couch\n',
    'etc/ssh/sshd_config.d/couch.conf': b'PermitRootLogin prohibit-password\nPasswordAuthentication no\nPubkeyAuthentication yes\nUseDNS no\n',
}


class StageError(ValueError):
    pass


def require(value, message):
    if not value:
        raise StageError(message)


def checksum(data):
    return hashlib.sha256(data).hexdigest()


def checked_file(path, expected):
    path = Path(path)
    require(not path.is_symlink() and path.is_file(), 'Expected a regular input file')
    require(isinstance(expected, str) and re.fullmatch('[0-9a-f]{64}', expected), 'Invalid input SHA-256')
    data = path.read_bytes()
    require(checksum(data) == expected, f'Input checksum mismatch: {path.name}')
    return data


def archive_name(name):
    require(isinstance(name, str) and '\\' not in name and not name.startswith('/'), 'Invalid archive path')
    parts = PurePosixPath(name).parts
    require('..' not in parts, 'Archive path traversal')
    return str(PurePosixPath(name))


# Reviewed set-ID files, by exact path, mode and content.
#
# A set-ID program runs with privileges its caller does not have, so staging
# refuses every one of them and a reviewed file has to be named here
# individually. `dbus-daemon-launch-helper` is D-Bus's system-bus activation
# helper: when a client asks the system bus for a service that is not running,
# this helper starts it under that service's own user account, which is why
# upstream ships it set-uid root with a group-restricted mode. Couch does not
# use D-Bus activation - it starts couch-bluetoothd itself, and nothing on the
# remote registers an activatable system service - so the helper sits unused.
# It is admitted because excluding it is worse, not because it is needed: it
# arrived with the Bluetooth packages in the OS closure, and the first-use path
# already installs this exact file on every remote where Bluetooth has been
# switched on (docs/bluetooth.md, "System packages on first use"), so refusing
# it in the image would leave the image's inventory different from every
# running remote's while changing nothing about what is installed. Pinning the
# digest means a future package revision of the same file fails this check and
# comes back for review. Any other set-ID file, and this path with a different
# mode or different bytes, still fails the build.
REVIEWED_SET_ID = {
    'usr/libexec/dbus-daemon-launch-helper': {
        'package': 'dbus-daemon-launch-helper-1.14.10-r4',
        'mode': 0o4750,
        'sha256': '26334163ead6299bdcb3f8b2e72ae3033cd80094ecdf731d7f934aaf623c782a',
    },
}


def reviewed_set_id(name, member, content):
    """True only for a reviewed set-ID file: exact path, mode and bytes.

    Callers consult this only when the set-ID bits are actually set, so an
    unreviewed set-ID file keeps failing and a listed path that carries no
    set-ID bits is never given a free pass by appearing here.
    """
    entry = REVIEWED_SET_ID.get(name)
    return (entry is not None and member.isreg() and member.mode == entry['mode']
            and checksum(content) == entry['sha256'])


def secret_path(name):
    parts = PurePosixPath(name).parts
    return ('.ssh' in parts or name.startswith(('home/', 'data/nvram/', 'opt/couch/vendor/', 'opt/couch/system/'))
        or name in ('etc/machine-id', 'var/lib/dbus/machine-id', 'etc/wpa_supplicant/wpa_supplicant.conf')
        or PurePosixPath(name).name in ('authorized_keys', 'networks.conf', 'settings.conf', 'props.tar.gz', '.ash_history', '.bash_history')
        or PurePosixPath(name).name.startswith('ssh_host_'))


def artifact_destination(name):
    name = archive_name(name)
    require(name not in GENERATED and not secret_path(name), 'Private or generated artifact destination')
    path = PurePosixPath(name)
    allowed = (path.parent == PurePosixPath('opt/couch') and
        (path.name in BINARIES or path.name.endswith('.sh') or path.name in ('update-key.pub', 'build.json', 'couch-wmt-properties.so')))
    allowed |= path.parent == PurePosixPath('opt/couch/licenses') and path.name in LICENSE_FILES
    allowed |= path.parent == PurePosixPath('opt/couch/www/cgi-bin') and path.name in RECOVERY_CGI
    allowed |= name.startswith('opt/couch/www/') and path.suffix in ('.html', '.css', '.js', '.svg', '.png', '.woff2', '.wasm')
    require(allowed, 'Artifact is not an explicitly supported Couch runtime file')
    return name


def build(spec, source_root=REPO):
    require(spec.get('schema') == 1, 'Unsupported staging schema')
    require(isinstance(spec.get('source_commit'), str) and re.fullmatch('[0-9a-f]{40}', spec['source_commit']), 'Record the complete source commit')
    epoch = spec.get('source_date_epoch')
    require(type(epoch) is int and 0 <= epoch <= 0xffffffff, 'Invalid reproducible timestamp')
    base = spec['alpine']
    require(base.get('architecture') == 'armv7', 'Only ARMv7 Alpine inputs are supported')
    require(isinstance(base.get('version'), str) and re.fullmatch(r'\d+\.\d+\.\d+', base['version']), 'Record the Alpine version')
    data = checked_file(base['path'], base['sha256'])
    entries = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:*') as archive:
        for member in archive:
            name = archive_name(member.name)
            if name == '.':
                continue
            require(name not in entries, 'Duplicate archive entry')
            require(member.isdir() or member.isreg() or member.issym() or member.islnk(), 'Special filesystem entry in base archive')
            content = archive.extractfile(member).read() if member.isreg() else b''
            require(not member.mode & 0o6000 or reviewed_set_id(name, member, content),
                    'Set-ID file requires separate review')
            require(not secret_path(name), f'Private/runtime state in clean base: {name}')
            require(not name.startswith('opt/couch/'), 'Base must not contain a preconfigured Couch installation')
            if name == 'etc/shadow':
                require(all(line.split(b':')[1] in (b'!', b'*', b'!!') for line in content.splitlines() if line), 'Base contains password credentials')
            link = member.linkname
            if member.issym() or member.islnk():
                require('\\' not in link, 'Invalid link path')
                # Absolute links are normal in Alpine: their root is the eventual
                # image root. Never follow them while preparing the host archive.
                target = posixpath.normpath(posixpath.join(posixpath.dirname(name) if member.issym() else '', link))
                require(target != '..' and not target.startswith('../'), 'Link escapes the image root')
            entries[name] = (tarfile.REGTYPE if member.isreg() else member.type, member.mode & 0o777, link, content)
    require('etc/alpine-release' in entries and entries['etc/alpine-release'][3].decode().strip() == base['version'], 'Alpine version differs from pinned input metadata')
    busybox = entries.get('bin/busybox', (None, 0, '', b''))[3]
    require(busybox[:6] == b'\x7fELF\x01\x01' and busybox[18:20] == b'\x28\x00', 'Base BusyBox is not little-endian 32-bit ARM ELF')
    if 'etc/shadow' in entries:
        kind, mode, link, shadow = entries['etc/shadow']
        # Allow later enrolled key authentication without shipping a password.
        shadow = re.sub(rb'(?m)^root:[^:]*:', b'root:*:', shadow)
        entries['etc/shadow'] = (kind, mode, link, shadow)
    inputs = []
    for artifact in spec.get('artifacts', []):
        name = artifact_destination(artifact['destination'])
        source = Path(source_root) / artifact['source']
        require(not Path(artifact['source']).is_absolute() and source.resolve().is_relative_to(Path(source_root).resolve()), 'Artifact source escapes checkout')
        content = checked_file(source, artifact['sha256'])
        if name == 'opt/couch/update-key.pub':
            require(re.fullmatch(rb'[0-9a-f]{64}\n?', content), 'Invalid public update trust key')
        if name == 'opt/couch/build.json':
            identity = json.loads(content)
            require(isinstance(identity, dict) and isinstance(identity.get('version'), str)
                    and re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?', identity['version']),
                    'Invalid runtime build identity')
        require(name not in entries, 'Artifact overwrites existing input')
        mode = artifact.get('mode', 0o755)
        require(mode in (0o644, 0o755), 'Unsupported artifact mode')
        if PurePosixPath(name).parent == PurePosixPath('opt/couch/www/cgi-bin'):
            require(mode == 0o755, 'Recovery CGI scripts must be executable')
        require(b'PRIVATE KEY-----' not in content and b'openssh-key-v1\0' not in content, 'Private key material in artifact')
        entries[name] = (tarfile.REGTYPE, mode, '', content)
        inputs.append({'source': artifact['source'], 'destination': name, 'sha256': artifact['sha256']})
    for name, content in GENERATED.items():
        entries[name] = (tarfile.REGTYPE, 0o644, '', content)
    for name in list(entries):
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.':
                entries.setdefault(str(parent), (tarfile.DIRTYPE, 0o755, '', b''))
    # No archive member may be written through a symlink/hardlink ancestor.
    for name in entries:
        for parent in PurePosixPath(name).parents:
            if str(parent) in entries:
                require(entries[str(parent)][0] == tarfile.DIRTYPE, 'Archive entry traverses a non-directory ancestor')
    for name, (kind, _, link, _) in entries.items():
        if kind == tarfile.LNKTYPE:
            target = archive_name(link)
            require(target in entries and entries[target][0] == tarfile.REGTYPE, 'Hardlink target must be a regular archive file')
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode='wb', filename='', mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, (kind, mode, link, content) in sorted(entries.items()):
                item = tarfile.TarInfo(name)
                item.type, item.mode, item.linkname = kind, mode, link
                item.uid = item.gid = 0
                item.uname = item.gname = ''
                item.mtime = epoch
                item.size = len(content) if kind == tarfile.REGTYPE else 0
                archive.addfile(item, io.BytesIO(content) if item.size else None)
    result = output.getvalue()
    manifest = {'schema': 1, 'kind': 'couch-clean-staging', 'installable': False,
        'source_commit': spec['source_commit'], 'source_date_epoch': epoch,
        'alpine': {key: base[key] for key in ('version', 'architecture', 'sha256')},
        'artifacts': inputs, 'archive_sha256': checksum(result), 'entries': len(entries),
        'pending': ['offline package closure', 'reviewed per-model vendor inputs',
            'raw partition images and observed layout', 'signed release manifest', 'hardware recovery validation']}
    return result, manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('spec', type=Path)
    parser.add_argument('output', type=Path, help='New output directory; never overwrites existing output')
    args = parser.parse_args()
    result, manifest = build(json.loads(args.spec.read_text()))
    args.output.mkdir(mode=0o755)
    (args.output / 'rootfs-staging.tar.gz').write_bytes(result)
    (args.output / 'staging.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
    print('Created noninstallable clean staging archive and source inventory.')


if __name__ == '__main__':
    main()
