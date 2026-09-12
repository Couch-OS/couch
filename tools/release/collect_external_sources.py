#!/usr/bin/env python3
"""Prepare kernel or Rust standard-library source-only receipts for release assembly."""
import argparse
import gzip
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import corresponding_source as source


def git_archive(repo, commit, path):
    if path.exists():
        raise ValueError('Use a fresh external source directory')
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('xb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
        process = subprocess.Popen(['git', '-C', str(repo), 'archive', '--format=tar', commit], stdout=subprocess.PIPE)
        shutil.copyfileobj(process.stdout, compressed, 1024 * 1024)
        if process.wait():
            raise ValueError('Cannot export pinned Git source')


def receipt(output, component, binary_sha, extra=None):
    files = source.tree_hashes(output)
    result = {'schema': 1, 'kind': 'couch-external-source', 'component': component,
              'source_archive': 'source.tar.gz', 'configuration': 'configuration.json',
              'build_recipe': 'BUILD.md', 'toolchain_receipt': 'toolchain.json',
              'binary_sha256': binary_sha, 'files': files, **(extra or {})}
    source.report(output / 'receipt.json', result)
    return result


def kernel(repo, build, couch, pin_file, output):
    pin = json.loads(pin_file.read_text())
    manifest = json.loads((build / 'manifest.json').read_text())
    for key in ('source_commit', 'compiler_sha256', 'container', 'profile'):
        if pin[key] != manifest.get(key):
            raise ValueError('Kernel receipt differs from release pin: ' + key)
    if manifest.get('source_status') or manifest.get('source_diff_sha256') != source.hashlib.sha256(b'').hexdigest():
        raise ValueError('Dirty kernel requires a separately audited matching patch; do not substitute the current tree')
    for relative, expected in [('.config', pin['config_sha256']), ('arch/arm/boot/zImage', pin['zimage_sha256'])]:
        if manifest['sha256'].get(relative) != expected or source.sha(build / relative) != expected:
            raise ValueError('Kernel configuration/binary differs from the selected receipt')
    commit = source.commit_id(repo, pin['source_commit'])
    git_archive(repo, commit, output / 'source.tar.gz')
    source.write(output / '.config', (build / '.config').read_bytes())
    source.write(output / 'configuration.json', json.dumps({'source_commit': commit, 'config_sha256': pin['config_sha256'], 'config_file': '.config', 'profile': pin['profile']}, indent=2).encode())
    source.write(output / 'toolchain.json', json.dumps(manifest, indent=2).encode())
    # Read recipes from the selected public Couch Git revision, not a build cache.
    exported = json.loads((couch.parent / 'project.json').read_text()) if (couch.parent / 'project.json').is_file() else None
    couch_commit = exported['commit'] if exported else source.commit_id(couch, 'HEAD')
    for relative in ('kernel/build.sh', 'kernel/Dockerfile', 'kernel/configure.py', 'kernel/source_policy.py', 'kernel/manifest.py', 'kernel/couch-ha100.config', 'kernel/profiles/normal.config'):
        if exported:
            if source.sha(couch / relative) != exported['files'].get(relative):
                raise ValueError('Exported kernel recipe differs from Git source inventory')
            data = (couch / relative).read_bytes()
        else:
            data = source.git(couch, 'show', couch_commit + ':' + relative)
        source.write(output / 'recipe' / relative, data)
    source.write(output / 'BUILD.md', (f'# Kernel build inputs\n\nSource is couch-kernel commit `{commit}`. Extract source.tar.gz to the kernel tree and use the exact `.config` and toolchain receipt here. The public Couch kernel recipe is retained under recipe/kernel; its Dockerfile pins the GCC prebuilt revision. The container content identifier and compiler hash are in toolchain.json. Run `make O=<output> ARCH=arm CROSS_COMPILE=arm-eabi- olddefconfig` and `make O=<output> ARCH=arm CROSS_COMPILE=arm-eabi- zImage` using that toolchain. Preserve the original source commit and Couch kernel build timestamp/localversion rules when comparing binary hashes.\n\nNo zImage, Android header, DTB, vendor firmware or calibration is included in this source bundle. The binary is identified only by SHA256.\n').encode())
    return receipt(output, 'kernel', pin['zimage_sha256'], {'source_commit': commit, 'couch_recipe_commit': couch_commit})


def rust_std(sysroot, output, rustc='rustc'):
    actual_sysroot = Path(subprocess.check_output([rustc, '--print', 'sysroot']).decode().strip()).resolve()
    if actual_sysroot != sysroot.resolve():
        raise ValueError('Rust source sysroot differs from the selected compiler')
    details = subprocess.check_output([rustc, '-Vv']).decode()
    fields = dict(line.split(': ', 1) for line in details.splitlines() if ': ' in line)
    tree = sysroot / 'lib/rustlib/src/rust'
    if not (tree / 'library/Cargo.toml').is_file():
        raise ValueError('Install rust-src for the exact build toolchain first')
    # Component source and library notices come from the same selected rustup sysroot.
    paths = source.tree_hashes(tree)
    output.mkdir(parents=True, exist_ok=True)
    with (output / 'source.tar.gz').open('xb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed, tarfile.open(fileobj=compressed, mode='w|') as archive:
        for name in sorted(paths):
            path = tree / name
            info = tarfile.TarInfo('rust-src/' + name); info.size = path.stat().st_size; info.mode = 0o644
            with path.open('rb') as stream: archive.addfile(info, stream)
    binary = list((sysroot / 'lib/rustlib/armv7-unknown-linux-musleabihf/lib').glob('libstd-*.rlib'))
    if len(binary) != 1:
        raise ValueError('Expected exactly one selected ARM Rust standard library')
    source.report(output / 'configuration.json', {'target': 'armv7-unknown-linux-musleabihf', 'release': fields['release'], 'commit': fields['commit-hash'], 'component_files': paths})
    source.report(output / 'toolchain.json', {'rustc_verbose': details, 'binary': binary[0].name, 'sha256': source.sha(binary[0]), 'component': 'rust-src'})
    docs = sysroot / 'share/doc/rust'
    for name in ('COPYRIGHT.html', 'COPYRIGHT-library.html', 'README.md'):
        source.write(output / 'notices' / name, (docs / name).read_bytes())
    for name in source.tree_hashes(docs / 'licenses'):
        source.write(output / 'notices/licenses' / name, (docs / 'licenses' / name).read_bytes())
    source.write(output / 'BUILD.md', (f'# Rust standard-library source\n\nThis is the rust-src component installed for Rust {fields["release"]}, commit {fields["commit-hash"]}, alongside the selected ARM standard-library artifact. Its Cargo manifests and library source are retained unchanged. The full compiler is a build tool, not distributed in this archive. To rebuild the standard library use the matching Rust compiler checkout and its bootstrap build instructions (`./x.py build library --target armv7-unknown-linux-musleabihf`); see https://rustc-dev-guide.rust-lang.org/building/how-to-build-and-run.html. The component sources alone are not a standalone compiler checkout. The Rust library copyright and license texts are under notices/.\n').encode())
    return receipt(output, 'rust-stdlib', source.sha(binary[0]), {'rust_release': fields['release'], 'rust_commit': fields['commit-hash']})


def main():
    parser = argparse.ArgumentParser(description=__doc__); sub = parser.add_subparsers(dest='kind', required=True)
    p = sub.add_parser('kernel'); p.add_argument('--repo', type=Path, required=True); p.add_argument('--build', type=Path, required=True); p.add_argument('--couch', type=Path, required=True); p.add_argument('--pin', type=Path, required=True); p.add_argument('--output', type=Path, required=True)
    p = sub.add_parser('rust-stdlib'); p.add_argument('--sysroot', type=Path, required=True); p.add_argument('--rustc', default='rustc'); p.add_argument('--output', type=Path, required=True)
    a = parser.parse_args()
    if a.kind == 'kernel': kernel(a.repo, a.build, a.couch, a.pin, a.output)
    else: rust_std(a.sysroot, a.output, a.rustc)


if __name__ == '__main__': main()
