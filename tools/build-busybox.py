#!/usr/bin/env python3
"""Build a traceable static ARMv7 boot BusyBox; verify before installing locally."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
RECIPE = ROOT / 'tools/busybox'
SOURCE_URL = 'https://busybox.net/downloads/busybox-1.37.0.tar.bz2'
SOURCE_SHA = '3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4'
# ARMv7 child of the existing Alpine 3.21.7 multi-architecture index.
IMAGE = 'alpine@sha256:e2d6b24023ccaac17dba05f7f085b89bd3a29eb4ee029f7ba71f5a0652ea0067'


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def require(value, message):
    if not value:
        raise ValueError(message)


def static_arm(path):
    data = path.read_bytes()
    require(len(data) >= 52 and data[:6] == b'\x7fELF\x01\x01'
            and struct.unpack_from('<H', data, 18)[0] == 40, 'Expected ARM32 little-endian ELF')
    require(struct.unpack_from('<I', data, 36)[0] & 0x400, 'Expected ARM hard-float ABI')
    offset = struct.unpack_from('<I', data, 28)[0]
    size, count = struct.unpack_from('<HH', data, 42)
    require(count > 0 and size >= 32 and offset + size * count <= len(data), 'Invalid ELF program headers')
    require(all(struct.unpack_from('<I', data, offset + n * size)[0] not in (2, 3)
                for n in range(count)), 'BusyBox must be static with no interpreter')


def verify(directory):
    directory = Path(directory)
    receipt = json.loads((directory / 'receipt.json').read_text())
    require(receipt.get('schema') == 1 and receipt.get('source_sha256') == SOURCE_SHA
            and receipt.get('builder_image') == IMAGE, 'BusyBox source/builder receipt differs')
    require(receipt['recipe_sha256'] == {p.name: digest(p) for p in RECIPE.iterdir() if p.is_file()},
            'BusyBox recipe changed; rebuild required')
    for name, checksum in receipt['artifacts'].items():
        require(Path(name).name == name and not (directory / name).is_symlink(), 'Invalid artifact path')
        require(digest(directory / name) == checksum, 'BusyBox build artifact changed')
    require({'busybox-armv7l', 'applets.txt', 'compiler.txt', 'installed-packages.txt',
             'busybox-source.tar.bz2', 'toolchain-closure.json', 'smoke.txt'} <= receipt['artifacts'].keys(),
            'Incomplete BusyBox build receipt')
    require(digest(directory / 'busybox-source.tar.bz2') == SOURCE_SHA, 'BusyBox source changed')
    static_arm(directory / 'busybox-armv7l')
    required = set((RECIPE / 'required-applets.txt').read_text().split())
    require(required <= set((directory / 'applets.txt').read_text().split()), 'Required boot applet missing')
    return receipt


def build(source, cache, output):
    sys.path.insert(0, str(ROOT / 'tools/release'))
    import package_closure
    source, cache, output = Path(source).resolve(), Path(cache).resolve(), Path(output).resolve()
    require(source.is_file() and not source.is_symlink() and digest(source) == SOURCE_SHA,
            'Expected pinned BusyBox source archive')
    closure = package_closure.verify(cache)
    require(closure['architecture'] == 'armv7', 'Expected ARMv7 compiler package closure')
    package_closure.authenticate(cache, closure)
    output.mkdir()  # Failed builds stay separate; never replace a prior binary.
    recipe = output / 'recipe'
    shutil.copytree(RECIPE, recipe)
    recipe_hashes = {p.name: digest(p) for p in recipe.iterdir() if p.is_file()}
    shutil.copyfile(source, output / 'busybox-source.tar.bz2')
    shutil.copyfile(cache / 'closure.json', output / 'toolchain-closure.json')
    subprocess.run(['docker', 'run', '--rm', '--platform=linux/arm/v7', '--network=none',
                    '--security-opt=no-new-privileges',
                    '--mount', f'type=bind,src={output},dst=/out',
                    '--mount', f'type=bind,src={cache},dst=/cache,readonly',
                    '--mount', f'type=bind,src={recipe},dst=/recipe,readonly',
                    IMAGE, 'sh', '/recipe/build.sh'], check=True)
    package_closure.verify(cache)
    names = ('busybox-armv7l', 'applets.txt', 'compiler.txt', 'installed-packages.txt',
             'busybox-source.tar.bz2', 'toolchain-closure.json', 'elf.txt', 'smoke.txt')
    receipt = {'schema': 1, 'source_url': SOURCE_URL, 'source_sha256': SOURCE_SHA,
               'builder_image': IMAGE, 'recipe_sha256': recipe_hashes,
               'artifacts': {name: digest(output / name) for name in names},
               'validation': 'ARM execution in pinned builder; applet inventory and static ELF check',
               'physical_boot_verified': False}
    (output / 'receipt.json').write_text(json.dumps(receipt, sort_keys=True, indent=2) + '\n')
    verify(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    b = commands.add_parser('build')
    for name in ('source', 'cache', 'output'):
        b.add_argument(name, type=Path)
    v = commands.add_parser('verify')
    v.add_argument('directory', type=Path)
    v.add_argument('--install', type=Path)
    args = parser.parse_args()
    if args.command == 'build':
        build(args.source, args.cache, args.output)
    else:
        verify(args.directory)
        if args.install:
            args.install.parent.mkdir(parents=True, exist_ok=True)
            temporary = args.install.with_name(args.install.name + '.pending')
            with temporary.open('xb') as target, (args.directory / 'busybox-armv7l').open('rb') as source:
                shutil.copyfileobj(source, target)
            temporary.chmod(0o755)
            temporary.replace(args.install)
    print('Verified static ARM BusyBox source build; physical boot validation is separate.')


if __name__ == '__main__':
    main()
