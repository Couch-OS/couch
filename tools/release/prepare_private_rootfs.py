#!/usr/bin/env python3
"""Add hash-verified static vendor files to a private, noninstallable image input."""
import argparse
import io
import json
import os
from pathlib import Path, PurePosixPath
import tarfile

from clean_stage import checksum, require
from prepare_rootfs import normalize
from installer_pins import load as load_installer_pin

_vendor = load_installer_pin('private_vendor')
verify_bundle, regular = _vendor.verify_bundle, _vendor.regular


def prepare(staging, vendor, output):
    require(not output.exists(), 'Private staging output must be new')
    bundle=verify_bundle(vendor)
    manifest=json.loads(regular(staging/'staging.json'))
    require(manifest.get('kind')=='couch-packaged-staging' and manifest.get('installable') is False,
            'Expected clean packaged staging input')
    archive_path=staging/'rootfs-staging.tar.gz'
    require(archive_path.is_file() and not archive_path.is_symlink(), 'Expected regular clean rootfs archive')
    source=archive_path.read_bytes()
    require(checksum(source)==manifest['archive_sha256'], 'Clean archive hash mismatch')
    epoch=manifest['source_date_epoch'];normalize(source,epoch)
    payloads={};members={}
    with tarfile.open(fileobj=io.BytesIO(source)) as archive:
        for item in archive:
            members[item.name]=item
            if item.isreg():payloads[item.name]=archive.extractfile(item).read()
    hashes={}
    for record in bundle['files']:
        name='opt/couch/'+record['path']
        require(name not in members, 'Private vendor overlay collides with clean runtime')
        for parent in PurePosixPath(name).parents:
            parent=str(parent)
            if parent=='.':continue
            if parent not in members:
                item=tarfile.TarInfo(parent);item.type=tarfile.DIRTYPE;item.mode=0o755;item.mtime=epoch;members[parent]=item
            require(members[parent].isdir(), 'Vendor overlay traverses an existing link')
        item=tarfile.TarInfo(name);item.mode=0o755 if '/bin/' in name else 0o644
        item.size=record['size'];item.mtime=epoch;members[name]=item
        payloads[name]=regular(vendor/record['path']);hashes[name]=record['sha256']
    raw=io.BytesIO()
    with tarfile.open(fileobj=raw,mode='w',format=tarfile.USTAR_FORMAT) as archive:
        for name,item in sorted(members.items()):
            archive.addfile(item,io.BytesIO(payloads[name]) if item.isreg() else None)
    data,count=normalize(raw.getvalue(),epoch,hashes)
    result={'schema':1,'kind':'couch-private-vendor-staging','installable':False,
            'private_only':True,'redistribution_authorized':False,'build_attested':False,
            'source_date_epoch':epoch,'archive_sha256':checksum(data),'entries':count,
            'clean_staging_sha256':checksum(regular(staging/'staging.json')),
            'vendor_provenance_sha256':checksum(regular(vendor/'vendor-provenance.json')),
            'pending':['vendor redistribution/source/notice review','clean source-to-binary attestation',
                       'boot/recovery image provenance and hardware validation','signed release inventory']}
    old=os.umask(0o077)
    try:
        output.mkdir(parents=True,mode=0o700)
        (output/'rootfs-staging.tar.gz').write_bytes(data)
        (output/'staging.json').write_text(json.dumps(result,sort_keys=True,indent=2)+'\n')
    finally:os.umask(old)
    return result

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('staging',type=Path);parser.add_argument('vendor',type=Path);parser.add_argument('output',type=Path)
    args=parser.parse_args();result=prepare(args.staging,args.vendor,args.output)
    print(f"Prepared private-only rootfs: {result['entries']} entries; installable=false")
