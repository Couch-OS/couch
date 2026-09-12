#!/usr/bin/env python3
"""Build a pinned, owner-local Python/MTK runtime; never open USB or include a DA."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import posixpath
import stat
import subprocess
import tarfile
import time
from urllib.parse import urlsplit
from urllib.request import Request, urlopen
import zipfile

HERE = Path(__file__).resolve().parent
PINS = HERE / 'mtk_dependencies.json'
INVENTORY = HERE.parent / 'installer/mtk_source_inventory.json'
MAX_FILE = 256 * 1024 * 1024
MAX_TOTAL = 2 * 1024 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def safe_name(name):
    path = PurePosixPath(name)
    require(name and not path.is_absolute() and '\\' not in name and ':' not in name
            and all(part not in ('', '.', '..') for part in name.split('/'))
            and all(ord(c) >= 32 for c in name), 'Unsafe runtime archive path')
    return name


def write_file(root, name, data, executable=False):
    target = root / safe_name(name)
    require(len(data) <= MAX_FILE, 'Runtime file exceeds bound')
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('xb') as stream:
        stream.write(data)
    target.chmod(0o755 if executable else 0o644)


def fetch(cache, pin, offline=False):
    target = cache / safe_name(pin['filename'])
    require(not target.is_symlink(), 'Symlink dependency cache refused')
    if target.exists():
        require(target.is_file() and target.stat().st_size == pin['size'] and sha(target) == pin['sha256'],
                'Cached dependency differs from reviewed pin')
        return target
    require(not offline, 'Pinned dependency absent from offline cache')
    url = urlsplit(pin['url'])
    require(url.scheme == 'https' and not url.username and not url.password and not url.fragment,
            'Dependency download requires public HTTPS')
    require(0 < pin['size'] <= MAX_FILE, 'Dependency exceeds size bound')
    cache.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name(target.name + '.download')
    stream = temporary.open('xb')
    deadline = time.monotonic() + 180
    try:
        with stream, urlopen(Request(pin['url'], headers={'User-Agent': 'Couch-installer-runtime/1'}), timeout=45) as response:
            require(urlsplit(response.url).scheme == 'https', 'Dependency redirected away from HTTPS')
            size = 0
            while True:
                require(time.monotonic() < deadline, 'Dependency download deadline exceeded')
                chunk = response.read1(1024 * 1024)
                require(time.monotonic() < deadline, 'Dependency download deadline exceeded')
                if not chunk:
                    break
                size += len(chunk)
                require(size <= pin['size'], 'Dependency exceeds pinned size')
                stream.write(chunk)
        require(size == pin['size'] and sha(temporary) == pin['sha256'], 'Downloaded dependency differs from reviewed pin')
        temporary.replace(target)
    finally:
        temporary.unlink(missing_ok=True)
    return target


def verified_snapshot(path, pin):
    """Parse only the exact pinned bytes, never reopen a previously checked path."""
    require(not path.is_symlink() and path.is_file(), 'Dependency snapshot must be a regular file')
    with path.open('rb') as stream:
        data = stream.read(MAX_FILE + 1)
    require(len(data) == pin['size'] and len(data) <= MAX_FILE
            and hashlib.sha256(data).hexdigest() == pin['sha256'],
            'Dependency changed before extraction')
    return data


def unpack_python(archive, output):
    """Flatten safe in-archive aliases, avoiding privileged Windows symlink creation."""
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        members = {}
        total = 0
        for member in source:
            if member.isdir():
                continue
            name = safe_name(member.name)
            require(name.startswith('python/') and name not in members, 'Unexpected Python archive member')
            require(member.isfile() or member.issym() or member.islnk(), 'Unexpected Python archive file type')
            require(0 <= member.size <= MAX_FILE, 'Python archive member exceeds bound')
            total += member.size
            require(total <= MAX_TOTAL and len(members) < 50000, 'Python archive exceeds extraction bound')
            members[name] = member
        for name, member in members.items():
            current, seen = member, set()
            while current.issym() or current.islnk():
                require(current.name not in seen and len(seen) < 16, 'Cyclic Python archive alias')
                seen.add(current.name)
                target = posixpath.normpath(posixpath.join(posixpath.dirname(current.name), current.linkname)) if current.issym() else current.linkname
                safe_name(target)
                require(target in members, 'Python archive alias escapes selected files')
                current = members[target]
            require(current.isfile(), 'Python archive alias does not target a file')
            data = source.extractfile(current).read(MAX_FILE + 1)
            write_file(output, name, data, bool(current.mode & 0o111))


def unpack_wheel(archive, output, site_packages):
    with zipfile.ZipFile(io.BytesIO(archive)) as source:
        total = 0
        for item in source.infolist():
            if item.is_dir():
                continue
            safe_name(item.orig_filename)
            name = safe_name(item.filename)
            mode = item.external_attr >> 16
            require(not stat.S_ISLNK(mode), 'Wheel symlink refused')
            require(item.file_size <= MAX_FILE, 'Wheel member exceeds bound')
            total += item.file_size
            require(total <= MAX_TOTAL, 'Wheel exceeds extraction bound')
            # These pinned wheels carry import packages and dist-info only. Do not
            # run installer hooks or silently accept future .data scheme changes.
            require(not any(part.endswith('.data') for part in PurePosixPath(name).parts), 'Wheel needs an unreviewed install scheme')
            write_file(output, site_packages + '/' + name, source.read(item), bool(mode & 0o111))


def unpack_mtk(archive, output, inventory):
    prefix = 'mtkclient-' + inventory['revision'] + '/'
    selected = set()
    with tarfile.open(fileobj=io.BytesIO(archive)) as source:
        for item in source:
            if not item.name.startswith(prefix):
                continue
            name = item.name[len(prefix):]
            if name not in inventory['files']:
                continue
            require(item.isfile() and name not in selected, 'Unexpected MTK source type or duplicate')
            expected = inventory['files'][name]
            require(item.size == expected['size'] and item.size <= MAX_FILE, 'MTK source size differs')
            data = source.extractfile(item).read()
            require(hashlib.sha256(data).hexdigest() == expected['sha256'], 'MTK source hash differs')
            require(name.endswith('.py') or name in ('LICENSE', 'README.md', 'pyproject.toml', 'requirements.txt'),
                    'MTK inventory admits non-source payload')
            write_file(output, 'mtk/' + name, data)
            selected.add(name)
    require(selected == set(inventory['files']), 'MTK archive lacks reviewed sources')


def file_inventory(root):
    result = {}
    for path in sorted(root.rglob('*')):
        require(not path.is_symlink(), 'Runtime symlink refused')
        if path.is_file():
            result[path.relative_to(root).as_posix()] = {'size': path.stat().st_size, 'sha256': sha(path)}
    return result


def prepare(platform_name, cache, output, offline=False):
    pins = json.loads(PINS.read_text()); pin = pins['platforms'][platform_name]
    require(not output.exists() and not output.is_symlink(), 'Use a new runtime output directory')
    require(not any((parent / '.git').exists() for parent in (output.resolve(), *output.resolve().parents)),
            'Runtime output must be outside Git checkouts')
    require(sha(INVENTORY) == pins['mtk_source']['inventory_sha256'], 'Reviewed MTK source inventory differs')
    inventory = json.loads(INVENTORY.read_text())
    # Verify every download before extracting or running an interpreter.
    python = fetch(cache, pin['python'], offline)
    wheels = [fetch(cache, wheel, offline) for wheel in pin['wheels']]
    mtk = fetch(cache, pins['mtk_source'], offline)
    python_bytes = verified_snapshot(python, pin['python'])
    wheel_bytes = [verified_snapshot(path, expected) for path, expected in zip(wheels, pin['wheels'])]
    mtk_bytes = verified_snapshot(mtk, pins['mtk_source'])
    output.mkdir(parents=True, mode=0o700)
    unpack_python(python_bytes, output)
    for wheel in wheel_bytes:
        unpack_wheel(wheel, output, pin['site_packages'])
    unpack_mtk(mtk_bytes, output, inventory)
    libraries = [p for p in (output / pin['site_packages'] / 'libusb_package').rglob('*')
                 if p.is_file() and ('libusb' in p.name.lower()) and p.suffix.lower() in ('.dll', '.so', '.dylib')]
    require(len(libraries) == 1, 'Pinned libusb wheel does not contain exactly one native library')
    files = file_inventory(output)
    require(pin['python_executable'] in files, 'Pinned Python executable absent')
    receipt = {'schema': 1, 'kind': 'couch-owner-mtk-runtime', 'platform': platform_name,
               'files': files, 'executables': {'python': pin['python_executable']},
               'site_packages': pin['site_packages'],
               'mtk': {'root': 'mtk', 'revision': inventory['revision'], 'source_inventory_sha256': sha(INVENTORY)},
               'native_libraries': {'libusb': libraries[0].relative_to(output).as_posix()},
               'pins_sha256': sha(PINS), 'inputs': [pin['python'], *pin['wheels'], pins['mtk_source']],
               'complete': True, 'loader_included': False, 'prebuilt_mtk_payloads_included': False,
               'physical_usb_validated': False}
    write_file(output, 'runtime.json', (json.dumps(receipt, indent=2, sort_keys=True) + '\n').encode())
    verify(output, sha(output / 'runtime.json'))
    return receipt


def verify(root, expected_receipt_sha256):
    require(root.is_dir() and not root.is_symlink(), 'Runtime root must be a real directory')
    receipt_path = root / 'runtime.json'
    require(not receipt_path.is_symlink() and receipt_path.is_file()
            and sha(receipt_path) == expected_receipt_sha256, 'Runtime receipt differs from trusted digest')
    receipt = json.loads(receipt_path.read_text())
    require(receipt.get('schema') == 1 and receipt.get('kind') == 'couch-owner-mtk-runtime'
            and receipt.get('complete') is True and receipt.get('loader_included') is False
            and receipt.get('prebuilt_mtk_payloads_included') is False, 'Invalid runtime receipt')
    require(receipt.get('pins_sha256') == sha(PINS), 'Runtime dependency pin version differs')
    actual = file_inventory(root); del actual['runtime.json']
    require(actual == receipt['files'], 'Runtime files differ from trusted receipt')
    for relative in [receipt['executables']['python'], receipt['native_libraries']['libusb']]:
        safe_name(relative); require(relative in actual, 'Runtime entrypoint absent from receipt')
    require(receipt['mtk']['source_inventory_sha256'] == sha(INVENTORY), 'Runtime source admission differs')
    return receipt


def host_platform():
    systems = {'Linux': 'linux', 'Darwin': 'macos', 'Windows': 'windows'}
    machines = {'x86_64': 'x86_64', 'AMD64': 'x86_64', 'arm64': 'aarch64', 'aarch64': 'aarch64'}
    return systems.get(platform.system(), 'unsupported') + '-' + machines.get(platform.machine(), 'unsupported')


def smoke(root, expected_receipt_sha256):
    receipt = verify(root, expected_receipt_sha256)
    require(receipt['platform'] == host_platform(), 'Smoke test requires the matching native platform')
    python = root / receipt['executables']['python']
    environment = {k: v for k, v in os.environ.items() if not k.startswith('PYTHON') and k not in ('LD_PRELOAD', 'LD_LIBRARY_PATH', 'DYLD_INSERT_LIBRARIES', 'DYLD_LIBRARY_PATH')}
    version = subprocess.run([str(python), '-I', '-B', '--version'], check=True, capture_output=True, text=True, timeout=20, env=environment).stdout.strip()
    require(version == 'Python ' + json.loads(PINS.read_text())['python_version'], 'Runtime Python version differs')
    code = '''import ctypes,json,pathlib,sys
sys.path.insert(0,sys.argv[1]);sys.path.insert(0,sys.argv[2])
import usb.core,usb.util,serial,colorama,Crypto.Cipher.AES,Cryptodome.Cipher.AES,libusb_package
library=pathlib.Path(libusb_package.get_library_path()).resolve()
assert library==pathlib.Path(sys.argv[3]).resolve()
ctypes.CDLL(str(library))
def no_usb(*a,**k): raise RuntimeError("USB discovery forbidden during import smoke")
usb.core.find=no_usb
from mtkclient.Library.mtk_class import Mtk
from mtkclient.Library.Connection.usblib import UsbClass
from mtkclient.config.mtk_config import MtkConfig
config=MtkConfig();config.interface=0;config.stock=True;config.loader=None
Mtk(config,preinit=False)
print(json.dumps({"imports":"passed","libusb_load":"passed","usb_opened":False,"mtk_preinit":False}))
'''
    result = subprocess.run([str(python), '-I', '-B', '-S', '-c', code, str(root / receipt['site_packages']),
                             str(root / receipt['mtk']['root']), str(root / receipt['native_libraries']['libusb'])],
                            check=True, capture_output=True, text=True, timeout=30, env=environment)
    verify(root, expected_receipt_sha256)
    return {'python': version, 'platform': receipt['platform'], 'receipt_sha256': expected_receipt_sha256,
            'smoke': json.loads(result.stdout.strip().splitlines()[-1]), 'physical_usb_validated': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__); sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('prepare'); p.add_argument('--platform', required=True, choices=json.loads(PINS.read_text())['platforms']); p.add_argument('--cache', type=Path, required=True); p.add_argument('--output', type=Path, required=True); p.add_argument('--offline', action='store_true')
    for command in ('verify', 'smoke'):
        p = sub.add_parser(command); p.add_argument('--root', type=Path, required=True); p.add_argument('--receipt-sha256', required=True)
    args = parser.parse_args()
    if args.command == 'prepare':
        prepare(args.platform, args.cache, args.output, args.offline)
        print(json.dumps({'root': str(args.output), 'receipt_sha256': sha(args.output / 'runtime.json')}))
    else:
        result = smoke(args.root, args.receipt_sha256) if args.command == 'smoke' else verify(args.root, args.receipt_sha256)
        print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
