#!/usr/bin/env python3
"""Prepare owner-held bootstrap and runtime inputs from the pinned official OTA."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import zipfile

import official_runtime
from private_vendor import file_sha, require

# These are distribution bytes, not per-device originals. The complete archive
# is pinned by official_runtime.PIN before any member is interpreted.
BOOTSTRAP_MEMBERS = {
    'preloader.img': (109992, '0ad0d14b7203d98a6567af7a022cfe5df5b6fcbba60cb4e9b4bc2ee569cf1069'),
    'boot.img': (8030464, 'dda78c8ebe7cb82095b08a10c2a1f779cbdbebc53464aee34c85bb3a7382cad7'),
    'odmdtbo.img': (37120, 'a5cf1159f6e8c0a95bd1d3b8c1edaca3b2c9704642df50912dfe52df277f0575'),
    'scatter.txt': (455, '531f0807ea065bed1c97a8a7d06284fa521749eadf3a6c0b149f0f2c72f8015d'),
}


def prepare(ota, output, *, runtime=True):
    ota, output = Path(ota), Path(output)
    pin = json.loads(official_runtime.PIN.read_text())
    require(not output.exists() and not output.is_symlink(), 'Use a new private output directory')
    require(not output.resolve().is_relative_to(Path(__file__).resolve().parents[2]),
            'Keep owner-held vendor inputs outside the source repository')
    require(ota.is_file() and not ota.is_symlink() and ota.stat().st_size == pin['size'],
            'Wrong regular official OTA file or size')
    require(file_sha(ota) == pin['sha256'], 'Official OTA archive hash mismatch')
    output.parent.mkdir(parents=True, exist_ok=True)
    old = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix='.couch-inputs-', dir=output.parent) as temporary:
            staged = Path(temporary) / 'inputs'
            staged.mkdir(mode=0o700)
            bootstrap = staged / 'bootstrap'
            bootstrap.mkdir(mode=0o700)
            files = {}
            with zipfile.ZipFile(ota) as archive:
                names = archive.namelist()
                require(len(names) == len(set(names)), 'Duplicate official OTA members')
                for name, (size, checksum) in BOOTSTRAP_MEMBERS.items():
                    data = bytearray()
                    official_runtime.checked_member(archive, name, {'size': size, 'sha256': checksum}, data.extend)
                    path = bootstrap / name
                    with path.open('xb') as target:
                        target.write(data)
                        target.flush()
                        os.fsync(target.fileno())
                    files[name] = {'size': size, 'sha256': checksum}
            if runtime:
                official_runtime.extract(ota, staged / 'vendor', pin)
            receipt = {'schema': 1, 'kind': 'owner-official-install-inputs',
                       'source_archive_sha256': pin['sha256'], 'version': pin['version'],
                       'bootstrap_files': files, 'vendor_runtime_prepared': runtime,
                       'private_only': True, 'redistribution_authorized': False,
                       'installable': False, 'original_device_backup': False,
                       'preloader_usage': 'EMI input only; never a flash target'}
            with (staged / 'inputs.json').open('x') as target:
                json.dump(receipt, target, indent=2)
                target.write('\n')
                target.flush()
                os.fsync(target.fileno())
            require(not output.exists() and not output.is_symlink(), 'Output appeared during preparation')
            staged.rename(output)
            return receipt
    finally:
        os.umask(old)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('official_zip', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--bootstrap-only', action='store_true', help='Skip runtime extraction for an enrollment-only check')
    args = parser.parse_args()
    prepare(args.official_zip, args.output, runtime=not args.bootstrap_only)
    print('Owner-held official inputs verified. No device access or installation authorization.')
