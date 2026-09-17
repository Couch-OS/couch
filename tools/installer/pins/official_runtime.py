#!/usr/bin/env python3
"""Offline, pinned official OTA to private runtime files. No downloads or devices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile
import zipfile

from private_vendor import ALLOWED, MAX_FILE, file_sha, read_image, require, sha, verify_bundle

PIN = Path(__file__).with_name('ha100_official_runtime.json')
BLOCK = 4096
MAX_IMAGE = 2 * 1024**3
MAX_TRANSFER = 128 * 1024


def transfer_ranges(data, image_size):
    require(len(data) <= MAX_TRANSFER and 0 < image_size <= MAX_IMAGE and image_size % BLOCK == 0,
            'Transfer/image size limit')
    lines = data.decode('ascii').splitlines()
    require(len(lines) >= 5 and lines[0] == '4' and lines[2:4] == ['0', '0'], 'Unsupported incremental/stash transfer')
    require(lines[1].isdigit(), 'Invalid total block count')
    limit = image_size // BLOCK
    new, all_ranges, total = [], [], 0
    for line in lines[4:]:
        words = line.split()
        require(len(words) == 2 and words[0] in ('new', 'zero', 'erase'), 'Unsupported transfer command')
        values = words[1].split(',')
        require(all(v.isdigit() for v in values), 'Non-numeric transfer range')
        values = [int(v) for v in values]
        require(values[0] == len(values)-1 and values[0] > 0 and values[0] % 2 == 0, 'Malformed range count')
        for start, end in zip(values[1::2], values[2::2]):
            require(0 <= start < end <= limit, 'Transfer range out of bounds')
            all_ranges.append((start, end))
            if words[0] != 'erase':
                total += end-start
            if words[0] == 'new':
                new.append((start*BLOCK, (end-start)*BLOCK))
    require(new and total == int(lines[1]), 'Transfer block count mismatch')
    ordered = sorted(all_ranges)
    require(all(a[1] <= b[0] for a, b in zip(ordered, ordered[1:])), 'Overlapping transfer ranges')
    return new


def checked_member(archive, name, expected, sink):
    info = archive.getinfo(name)
    require(info.file_size == expected['size'] and not info.is_dir(), 'ZIP member size mismatch')
    require((info.external_attr >> 16) & 0o170000 != 0o120000, 'ZIP member symlink')
    digest, size = hashlib.sha256(), 0
    with archive.open(info) as source:
        while block := source.read(4096):
            size += len(block)
            require(size <= expected['size'], 'ZIP member overflow')
            digest.update(block)
            sink(block)
    require(size == expected['size'] and digest.hexdigest() == expected['sha256'], 'ZIP member hash mismatch')


def reconstruct(archive, role, pin, output, decoder_factory=None):
    if decoder_factory is None:
        import brotli
        decoder_factory = brotli.Decompressor
    transfer = bytearray()
    checked_member(archive, role+'.transfer.list', pin['members'][role+'.transfer.list'], transfer.extend)
    size = pin['images'][role]['size']
    ranges = transfer_ranges(transfer, size)
    expected_bytes = sum(length for _, length in ranges)
    decoder, index, used, received = decoder_factory(), 0, 0, 0
    with output.open('xb') as image:
        image.truncate(size)
        def consume(chunk):
            nonlocal index, used, received
            # Small compressed input chunks bound each decoder invocation; the
            # exact authenticated archive and expected total bound all output.
            data = decoder.process(chunk)
            received += len(data)
            require(received <= expected_bytes, 'Brotli output exceeds transfer size')
            offset = 0
            while offset < len(data):
                require(index < len(ranges), 'Unexpected decompressed payload')
                start, length = ranges[index]
                amount = min(length-used, len(data)-offset)
                image.seek(start+used)
                image.write(data[offset:offset+amount])
                offset += amount
                used += amount
                if used == length:
                    index += 1
                    used = 0
        checked_member(archive, role+'.new.dat.br', pin['members'][role+'.new.dat.br'], consume)
        require(decoder.is_finished() and received == expected_bytes and index == len(ranges) and used == 0,
                'Truncated Brotli or transfer payload')
    require(file_sha(output) == pin['images'][role]['sha256'], 'Reconstructed image hash mismatch')


def extract(ota, output, pin):
    require(not output.exists() and not output.is_symlink(), 'Private output must be new')
    require(ota.is_file() and not ota.is_symlink() and ota.stat().st_size == pin['size'], 'Wrong regular OTA file/size')
    require(file_sha(ota) == pin['sha256'], 'Official OTA archive hash mismatch')
    require(pin['schema'] == 1 and {r['path'] for r in pin['files']} == ALLOWED and len(pin['files']) == len(ALLOWED),
            'Pinned file inventory mismatch')
    require(shutil.disk_usage(output.parent).free >= sum(r['size'] for r in pin['images'].values()) + 128*1024**2,
            'Insufficient scratch space')
    old = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix='.couch-official-', dir=output.parent) as scratch:
            scratch = Path(scratch)
            bundle = scratch / 'bundle'
            bundle.mkdir()
            images = {}
            with zipfile.ZipFile(ota) as archive:
                names = archive.namelist()
                require(len(names) == len(set(names)), 'Duplicate ZIP members')
                for role in ('system', 'vendor'):
                    image = scratch / (role+'.img')
                    reconstruct(archive, role, pin, image)
                    images[role] = image
            records = []
            for record in pin['files']:
                name = record['path']
                require(0 < record['size'] <= MAX_FILE, 'Pinned runtime file size limit')
                content = read_image(images[name.split('/')[0]], name)
                require(len(content) == record['size'] and sha(content) == record['sha256'], 'Official runtime file mismatch')
                target = bundle / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
                records.append({**record, 'source_image': name.split('/')[0]+'.img',
                                'source_path': '/'+name.split('/',1)[1], 'operation': 'extracted pinned official OTA file',
                                'redistribution': 'unreviewed; private use only'})
            manifest = {'schema':1, 'kind':'couch-private-vendor-bundle', 'installable':False,
                        'private_only':True, 'redistribution_authorized':False, 'build_attested':False,
                        'source_images':pin['images'], 'source_origin':'User-supplied pinned official OTA; offline local extraction',
                        'source_archive':{'sha256':pin['sha256'], 'size':pin['size'], 'version':pin['version']},
                        'extractor':{'name':'official_runtime.py + debugfs', 'mode':'read-only files; no mounts'},
                        'files':records, 'missing':[]}
            (bundle/'vendor-provenance.json').write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n')
            verify_bundle(bundle)
            require(not output.exists(), 'Output appeared during preparation')
            bundle.rename(output)
    finally:
        os.umask(old)
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('official_zip', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    result = extract(args.official_zip, args.output, json.loads(PIN.read_text()))
    print(f"Verified {len(result['files'])} private runtime files; installable=false; redistribution not authorized")
