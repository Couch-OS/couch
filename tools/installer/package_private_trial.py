#!/usr/bin/env python3
"""Package an already prepared private trial; no publishing or device access."""
import argparse
import gzip
import hashlib
from pathlib import Path
import tarfile
from urllib.parse import urlparse


def package(payload, output, base_url):
    payload=Path(payload).resolve();output=Path(output)
    parsed=urlparse(base_url)
    if not (parsed.scheme=='https' or (parsed.scheme=='http' and parsed.hostname=='127.0.0.1')) or parsed.query or parsed.fragment:
        raise ValueError('Use HTTPS or explicit loopback for a private trial')
    if output.exists():raise ValueError('Use a new private package directory')
    for name in ('trial.json','requirements.txt','tools/installer/couch_tui.py','tools/installer/ratatui_backend.py','bin/couch-installer-tui'):
        if not (payload/name).is_file():raise ValueError('Missing trial package input: '+name)
    with (payload/'bin/couch-installer-tui').open('rb') as binary:
        header=binary.read(20)
    if header[:6]!=b'\x7fELF\x02\x01' or header[18:20]!=b'\x3e\x00':
        raise ValueError('Expected Linux x86_64 Ratatui executable')
    paths=sorted(payload.rglob('*'))
    if any(p.is_symlink() or not(p.is_file() or p.is_dir()) for p in paths):
        raise ValueError('Package must contain only regular files/directories')
    output.mkdir(parents=True,mode=0o700)
    archive=output/'couch-installer.tar.gz'
    with archive.open('wb') as file, gzip.GzipFile(fileobj=file,mode='wb',compresslevel=1,mtime=0) as compressed:
        with tarfile.open(fileobj=compressed,mode='w|',format=tarfile.USTAR_FORMAT) as bundle:
            for path in [payload,*paths]:
                name='couch-installer'+('' if path==payload else '/'+path.relative_to(payload).as_posix())
                info=bundle.gettarinfo(str(path),arcname=name)
                info.uid=info.gid=info.mtime=0;info.uname=info.gname=''
                if path.is_file():
                    # Do not encode hardlinks: the verifier accepts regular files only.
                    info.type=tarfile.REGTYPE;info.linkname='';info.size=path.stat().st_size
                    with path.open('rb') as source:bundle.addfile(info,source)
                else:bundle.addfile(info)
    with archive.open('rb') as file:checksum=hashlib.file_digest(file,'sha256').hexdigest()
    script=Path(__file__).with_name('private_curl.sh.in').read_text()
    url=base_url.rstrip('/')+'/couch-installer.tar.gz'
    if "'" in url or any(ord(c)<32 for c in url):raise ValueError('Invalid package URL')
    (output/'install.sh').write_text(script.replace('@PACKAGE_URL@',url).replace('@PACKAGE_SHA256@',checksum))
    (output/'SHA256SUMS').write_text(checksum+'  couch-installer.tar.gz\n')
    return checksum


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--payload',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--base-url',required=True)
    args=parser.parse_args()
    print(package(args.payload,args.output,args.base_url))
