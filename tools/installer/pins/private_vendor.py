#!/usr/bin/env python3
"""Private, offline vendor recovery. Never contacts a device or grants redistribution."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

MISSING = ('vendor/etc/selinux/nonplat_property_contexts',
           'system/etc/selinux/plat_property_contexts', 'system/etc/ld.config.txt')
STATIC = ('vendor/lib/modules/wmt_drv.ko', 'vendor/lib/modules/wmt_chrdev_wifi.ko',
          'vendor/lib/modules/wlan_drv.ko', 'vendor/bin/wmt_loader', 'vendor/bin/wmt_launcher',
          'system/bin/linker', *('system/lib/'+name for name in
          ('libc.so','libm.so','libdl.so','libc++.so','liblog.so','libcutils.so',
           'libbacktrace.so','libunwind.so','libutils.so','libnetd_client.so','libbase.so','liblzma.so')))
# This explicit list is the original bundle's complete static firmware set;
# no NVRAM, device properties or calibration directories are accepted.
FIRMWARE = tuple('vendor/firmware/'+name for name in (
    'WMT_SOC.cfg','WIFI_RAM_CODE_6580','pcm_sodi.bin','ROMv2_lm_patch_1_0_hdr.bin',
    'ROMv2_lm_patch_1_1_hdr.bin','catcher_filter_1_wg_n.bin','pcm_suspend.bin',
    'soc_fm_v1_patch.bin','soc_fm_v1_coeff.bin','fm_cust.cfg','pcm_deepidle.bin','modem_1_wg_n.img'))
ALLOWED = frozenset((*STATIC,*FIRMWARE,*MISSING))
MAX_FILE = 32*1024*1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    digest=hashlib.sha256()
    with path.open('rb') as source:
        while block:=source.read(1024*1024):
            digest.update(block)
    return digest.hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), f'Expected regular input: {path.name}')
    require(path.stat().st_size <= MAX_FILE, 'Vendor input exceeds 32 MiB')
    return path.read_bytes()


def read_image(image, name):
    require(name in ALLOWED, 'Not an allowed static vendor path')
    relative=name.split('/',1)[1]
    result=subprocess.run(['debugfs','-R',f'cat /{relative}',str(image)],capture_output=True,timeout=30,check=True)
    require(0 < len(result.stdout) <= MAX_FILE, f'Missing/oversized image member: {name}')
    return result.stdout


def extract(base, system, vendor, hashes, output):
    require(not output.exists(), 'Private output must be new')
    require(not system.is_symlink() and not vendor.is_symlink(), 'Source image must not be a symlink')
    images={'system':system.resolve(),'vendor':vendor.resolve()}
    for role,image in images.items():
        require(image.is_file() and not image.is_symlink(), f'Missing regular {role} image')
        require(re.fullmatch('[0-9a-f]{64}',hashes[role]) and file_sha(image)==hashes[role], f'{role} image hash mismatch')
        with image.open('rb') as source:
            source.seek(1080)
            require(source.read(2)==b'\x53\xef', f'{role} image is not raw ext4')
    require(base.is_dir() and not base.is_symlink(), 'Expected private base bundle directory')
    existing={}
    for path in base.rglob('*'):
        require(not path.is_symlink(), 'Vendor bundle must not contain symlinks')
        if path.is_file():
            name=path.relative_to(base).as_posix()
            require(name in ALLOWED, f'Unreviewed vendor path: {name}')
            existing[name]=regular(path)
    require(set(existing) >= set(STATIC+FIRMWARE), 'Existing static bundle is incomplete')
    payload={};records=[]
    for name in sorted(ALLOWED):
        role=name.split('/',1)[0]
        original=read_image(images[role],name)
        if name in existing:
            require(existing[name]==original, f'Existing bundle differs from original image: {name}')
            content=existing[name]
        else:
            require(name in MISSING, 'Only missing property/linker configuration can be extracted')
            content=original
        payload[name]=content
        records.append({'path':name,'size':len(content),'sha256':sha(content),
                        'source_image':role+'.img','source_path':'/'+name.split('/',1)[1],
                        'operation':'verified existing file' if name in existing else 'extracted missing file',
                        'redistribution':'unreviewed; private use only'})
    version=subprocess.run(['debugfs','-V'],capture_output=True,text=True,check=True).stderr.strip()
    manifest={'schema':1,'kind':'couch-private-vendor-bundle','installable':False,
              'private_only':True,'redistribution_authorized':False,
              'source_images':{r:{'filename':r+'.img','size':p.stat().st_size,'sha256':hashes[r]} for r,p in images.items()},
              'source_origin':'Original Android partition backups supplied by device owner; no device access',
              'extractor':{'name':'debugfs','version':version,'mode':'read-only cat; no mounts'},
              'files':records,'missing':[],'build_attested':False}
    old=os.umask(0o077)
    try:
        output.mkdir(parents=True,mode=0o700)
        for name,content in payload.items():
            path=output/name;path.parent.mkdir(parents=True,exist_ok=True)
            path.write_bytes(content);path.chmod(0o600)
        (output/'vendor-provenance.json').write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n')
    finally:
        os.umask(old)
    return manifest


def verify_bundle(directory):
    manifest=json.loads(regular(directory/'vendor-provenance.json'))
    require(manifest.get('schema') == 1 and manifest.get('installable') is False and manifest.get('kind')=='couch-private-vendor-bundle' and manifest.get('private_only') is True and
            manifest.get('redistribution_authorized') is False, 'Expected explicitly private vendor provenance')
    records=manifest['files'];require({r['path'] for r in records}==ALLOWED and len(records)==len(ALLOWED), 'Incomplete/duplicate vendor inventory')
    actual={p.relative_to(directory).as_posix() for p in directory.rglob('*') if p.is_file()}
    require(actual==ALLOWED|{'vendor-provenance.json'}, 'Uninventoried private bundle file')
    for path in directory.rglob('*'):
        require(not path.is_symlink(), 'Symlink in private vendor bundle')
    for record in records:
        data=regular(directory/record['path'])
        require(len(data)==record['size'] and sha(data)==record['sha256'], 'Vendor bundle hash mismatch')
    return manifest


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base',type=Path);parser.add_argument('output',type=Path)
    parser.add_argument('--system-image',type=Path,required=True);parser.add_argument('--vendor-image',type=Path,required=True)
    parser.add_argument('--system-sha256',required=True);parser.add_argument('--vendor-sha256',required=True)
    args=parser.parse_args()
    result=extract(args.base,args.system_image,args.vendor_image,{'system':args.system_sha256,'vendor':args.vendor_sha256},args.output)
    print(f"Private bundle verified: {len(result['files'])} files; redistribution not authorized")

if __name__=='__main__':
    main()
