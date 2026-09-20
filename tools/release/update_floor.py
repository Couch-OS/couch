#!/usr/bin/env python3
"""Check what a runtime bundle publishes against the oldest deployed updater.

A runtime bundle is refused *before download* by any updater whose allowlist
does not know every file name in the signed manifest, and the updater offers
only the single newest release on the channel: it does not fall back to an
older one. So one bundle carrying one unknown name strands every remote whose
updater predates that name, on every release after it, until a full OS
reinstall. That is not a theory - it happened twice, see docs/runtime-updates.md
("Compatibility floor").

This module encodes that allowlist once, from the oldest updater that is
actually deployed, and the release tooling refuses to publish a bundle that
would fail it.
"""
import argparse
import json
from pathlib import Path
import sys

# THE COMPATIBILITY FLOOR.
#
# The public installer writes an OS image whose bundled runtime - and therefore
# whose updater - is this release. A remote installed today runs it until it
# updates itself, so it is the oldest updater in the field and every published
# runtime bundle has to satisfy it.
#
# The rules below are a transcription of `allowed()` and `inventory()` in
# daemon/couch-updates/src/staging.rs AT THAT TAG:
#
#     git show v0.1.0-alpha.20260910.24:daemon/couch-updates/src/staging.rs
#
# Do not relax them to match the updater in this checkout. Today's updater
# accepts any top-level `couch-*` executable (added in .142); the floor's does
# not, and a bundle is only installable if the FLOOR accepts it.
#
# THE FLOOR CANNOT MOVE until the installer's OS image is rebuilt with a newer
# runtime AND every remote already installed from the current image has been
# updated past it. Rebuilding the image is the real fix; it is a known task in
# docs/installer.md. Until then, a new binary rides in the boot ramdisk
# (/extra, see tools/release/prepare_boot_candidates.py), not in the bundle.
FLOOR_RELEASE = 'v0.1.0-alpha.20260910.24'
FLOOR_SOURCE = f'{FLOOR_RELEASE}:daemon/couch-updates/src/staging.rs'

# staging.rs REQUIRED at the floor. Identical to REQUIRED in the current
# staging.rs; test_update_floor.py asserts that, because the publisher builds
# its file list from the current one.
REQUIRED = ('couch-gui', 'couch-confd', 'couch-system', 'couch-sonos', 'couch-coreelec',
            'couch-wmt-properties.so', 'stage2.sh', 'hardware-init.sh', 'gui-start.sh',
            'system.sh', 'confd.sh', 'setup-mode.sh', 'portal.sh', 'station.sh',
            'wifi-conf.sh', 'build.json')
# Optional extras the floor's allowlist tolerates.
CONSOLE = 'fbcon'
CGI = ('cgi-bin/save', 'cgi-bin/scan', 'cgi-bin/enroll', 'cgi-bin/setpw')
WEB_SUFFIXES = ('.html', '.css', '.js', '.svg', '.png', '.woff2')
MAX_NAME = 180
MAX_FILES = 256
MAX_FILE = 64 * 1024 * 1024
MAX_TOTAL = 128 * 1024 * 1024


# What a couch-confd built with protocol 3 switched on carries in its bytes
# (daemon/couch-confd/src/assets.rs; docs/development/protocol.md, "The
# switch"). A Cargo feature shows in no file name, tree hash or version, so the
# line is the only thing that tells such a daemon from an ordinary one. It is
# for one development remote, and couch_updates::bundle signs it only under a
# `.p3.dev` version; the check here says so before the seed is ever read.
PREVIEW_MARKERS = {'protocol-3': b'COUCH-PREVIEW-BUILD protocol-3'}


def preview_features(daemon):
    """The preview features a couch-confd's raw bytes say it was built with."""
    return sorted(name for name, marker in PREVIEW_MARKERS.items() if marker in (daemon or b''))


def tree_preview_features(tree):
    """The same for a staged runtime tree; a tree with no daemon has none."""
    path = Path(tree) / 'couch-confd'
    return preview_features(path.read_bytes()) if path.is_file() and not path.is_symlink() else []


def _traversable(name):
    """The floor's path check: every component Normal, no backslash."""
    if not name or len(name) > MAX_NAME or '\\' in name or name.startswith('/'):
        return False
    parts = [p for p in name.split('/') if p != '.' or name.startswith('./')]
    return bool(parts) and all(p not in ('', '.', '..') for p in parts)


def allowed(name):
    """Exactly `allowed()` in staging.rs at FLOOR_RELEASE.

    Note what IS here: `licenses/*.txt`. A report that the licence texts were
    what the floor refused was checked against the tag and is wrong; the floor
    has always taken them. What is NOT here is any `couch-*` wildcard.
    """
    if not _traversable(name):
        return False
    if name in REQUIRED or name == CONSOLE:
        return True
    if name.startswith('www/'):
        rest = name[len('www/'):]
        if rest.startswith('.'):
            return False
        if rest.startswith('cgi-bin/'):
            return rest in CGI
        return rest.endswith(WEB_SUFFIXES)
    return name.startswith('licenses/') and name.endswith('.txt')


def must_be_executable(name):
    """The floor's mode rule for scripts, Couch binaries and the recovery CGI."""
    return name.endswith('.sh') or name.startswith('couch-') or name.startswith('www/cgi-bin/')


def problems(files, kind='runtime', installable=True):
    """Every reason the floor's updater would refuse this bundle, in order.

    `files` is the manifest's file list: dicts with path/size/mode (sha256
    optional here - the floor checks it, but a name list is what regresses).
    """
    found = []
    if kind != 'runtime':
        found.append(f'kind is {kind!r}, not "runtime"')
    if not installable:
        found.append('manifest is not marked installable')
    if len(files) > MAX_FILES:
        found.append(f'{len(files)} files; the floor accepts at most {MAX_FILES}')
    seen, total = set(), 0
    for item in sorted(files, key=lambda f: f['path']):
        path, size, mode = item['path'], int(item.get('size', 0)), item.get('mode')
        if not allowed(path):
            found.append(f'{path}: not on the {FLOOR_RELEASE} allowlist')
        if path in seen:
            found.append(f'{path}: duplicate path')
        seen.add(path)
        if mode is not None and mode not in (0o644, 0o755):
            found.append(f'{path}: mode {mode:o} is neither 0644 nor 0755')
        elif mode is not None and must_be_executable(path) and mode != 0o755:
            found.append(f'{path}: must be mode 0755')
        if size > MAX_FILE:
            found.append(f'{path}: {size} bytes exceeds the {MAX_FILE}-byte file limit')
        total += size
    for name in REQUIRED:
        if name not in seen:
            found.append(f'{name}: required by the floor and missing')
    if total > MAX_TOTAL:
        found.append(f'{total} bytes in total exceeds the {MAX_TOTAL}-byte bundle limit')
    return found


def manifest_files(document):
    """The file list of a signed (`{"signed": ...}`) or bare update manifest."""
    manifest = document.get('signed', document)
    if not isinstance(manifest, dict) or 'files' not in manifest:
        raise ValueError('Not an update manifest: no file list')
    return manifest


def bundle_names(tree):
    """Exactly the names `couch_updates::bundle` would publish from this tree.

    The publisher takes REQUIRED, every further top-level `couch-*` regular
    file, and everything under www/ and licenses/. Anything else in a clean
    runtime export (os-baseline.json, update-key.pub, runtime-boot.sh) stays
    out of the bundle, so it is not the floor's business.
    """
    tree = Path(tree)
    names = set(REQUIRED)
    for entry in sorted(tree.iterdir()):
        if entry.name.startswith('couch-') and entry.is_file() and not entry.is_symlink():
            names.add(entry.name)
    for directory in ('www', 'licenses'):
        base = tree / directory
        if not base.is_dir():
            continue
        for path in sorted(base.rglob('*')):
            if path.is_file() and not path.is_symlink():
                names.add(path.relative_to(tree).as_posix())
    return sorted(names)


def tree_files(tree):
    """A manifest-shaped file list for a staged runtime tree."""
    tree, files = Path(tree), []
    for name in bundle_names(tree):
        path = tree / name
        # build.json is generated by the publisher; an export need not carry one.
        size = path.stat().st_size if path.is_file() else 0
        files.append({'path': name, 'size': size,
                      'mode': 0o755 if must_be_executable(name) else 0o644})
    return files


def inventory_files():
    """The bundle a promotion from this checkout would publish, from the lists.

    Mirrors `bundle_names` without needing built binaries: REQUIRED, the
    top-level `couch-*` files tools/release/runtime_inventory.py stages into
    /opt/couch, the web assets and the licence texts. Two things are staged
    into /opt/couch but not published, exactly as the publisher has it: `fbcon`
    (not a `couch-*` name) and `runtime-boot.sh` (the stable bootstrap, which
    only a full OS image may change). `BOOT_EXTRA` is not here either, and that
    is the point - those names go in the boot ramdisk.
    """
    import runtime_inventory

    names = set(REQUIRED) | {'www/index.html'}
    names |= {name for name in runtime_inventory.RUNTIME if name.startswith('couch-')}
    names |= {name for name in runtime_inventory.RUNTIME_ALPINE if name.startswith('couch-')}
    names |= {'www/cgi-bin/' + name for name in runtime_inventory.CGI}
    names |= {'licenses/' + name for name in runtime_inventory.LICENSES}
    return [{'path': name, 'size': 0,
             'mode': 0o755 if must_be_executable(name) else 0o644}
            for name in sorted(names)]


def report(files, stream=sys.stdout, label=''):
    """Print the bundle contents and the verdict. True when the floor takes it."""
    found = problems(files)
    print(f'{label or "bundle"}: {len(files)} files, '
          f'{sum(int(f.get("size", 0)) for f in files)} bytes', file=stream)
    for item in sorted(files, key=lambda f: f['path']):
        mark = ' ' if allowed(item['path']) else 'X'
        print(f'  {mark} {item["path"]}', file=stream)
    if found:
        print(f'\nREFUSED by the {FLOOR_RELEASE} updater ({FLOOR_SOURCE}):', file=stream)
        for reason in found:
            print(f'  - {reason}', file=stream)
        print('\nA remote on that release cannot install this bundle, and the updater\n'
              'offers only the newest release, so it stays on the version it has.\n'
              'Ship the new file in the boot ramdisk instead (/extra); see\n'
              'docs/runtime-updates.md, "Compatibility floor".', file=stream)
    else:
        print(f'\nOK: installable by the {FLOOR_RELEASE} updater.', file=stream)
    return not found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--manifest', type=Path,
                        help='A signed or bare couch-VERSION-ha100-update.json')
    source.add_argument('--tree', type=Path,
                        help='A clean runtime tree, checked as the publisher would bundle it')
    source.add_argument('--inventory', action='store_true',
                        help="This checkout's runtime payload lists, with no build needed")
    parser.add_argument('--allow-preview', action='store_true',
                        help='With --tree: accept a couch-confd built with protocol 3 switched on. '
                             'Only for a .p3.dev build that goes to one development remote')
    args = parser.parse_args(argv)
    # sys.stdout is named at each call: report()'s default was bound at import.
    if args.allow_preview and not args.tree:
        parser.error('--allow-preview only means something with --tree')
    if args.manifest:
        manifest = manifest_files(json.loads(args.manifest.read_text()))
        ok = report(manifest['files'], sys.stdout, manifest.get('version', str(args.manifest)))
    elif args.tree:
        ok = report(tree_files(args.tree), sys.stdout, str(args.tree))
        preview = tree_preview_features(args.tree)
        if preview:
            print(f'\nPREVIEW BUILD: couch-confd in this tree was built with {", ".join(preview)} '
                  'switched on.\nIt is for one development remote: Dev channel, a version ending '
                  '.p3.dev, never Alpha.\ncouch-updates refuses to sign it under any other version '
                  '(docs/development/protocol.md, "The switch").')
            if not args.allow_preview:
                print('REFUSED: pass --allow-preview if that is what this tree is for.')
                ok = False
    else:
        ok = report(inventory_files(), sys.stdout, 'tools/release/runtime_inventory.py')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
