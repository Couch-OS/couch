#!/usr/bin/env python3
"""Inspect private vendor ELF dependencies without executing vendor binaries."""
import argparse
import json
from pathlib import Path
import re
import subprocess

from installer_pins import load as load_installer_pin

_vendor = load_installer_pin('private_vendor')
regular, require, verify_bundle = _vendor.regular, _vendor.require, _vendor.verify_bundle


def closure(records, roots, virtual):
    by_name={Path(r['path']).name:r for r in records}
    required=set();missing=set();pending=list(roots)
    while pending:
        name=pending.pop()
        if name in required or name in virtual:continue
        required.add(name)
        if name not in by_name:missing.add(name);continue
        pending.extend(by_name[name]['needed'])
    all_missing={needed for r in records for needed in r['needed'] if needed not in by_name and needed not in virtual}
    return {'wmt_dependency_names':sorted(required),'missing_required':sorted(missing),
            'missing_outside_wmt_closure':sorted(all_missing-missing)}


def audit(bundle):
    manifest=verify_bundle(bundle);records=[]
    for item in manifest['files']:
        name=item['path'];data=regular(bundle/name)
        if data[:4]!=b'\x7fELF':continue
        result=subprocess.run(['readelf','-d',str(bundle/name)],capture_output=True,text=True,check=True,timeout=30)
        records.append({'path':name,'sha256':item['sha256'],
                        'needed':re.findall(r'Shared library: \[([^]]+)\]',result.stdout)})
    # Android O's linker constructs this pseudo-library in get_libdl_info().
    # Require its name in the exact inventoried linker; do not accept it globally.
    virtual=['ld-android.so'] if b'ld-android.so\0' in regular(bundle/'system/bin/linker') else []
    return {'schema':1,'kind':'couch-private-vendor-elf-audit','installable':False,'private_only':True,
            'runtime_loading_tested':False,'linker_virtual_libraries':virtual,
            'linker_reference':'https://android.googlesource.com/platform/bionic/+/refs/heads/oreo-r4-release/linker/dlfcn.cpp',
            'method':'readelf DT_NEEDED graph; no vendor process or device access',
            'files':records,**closure(records,['wmt_loader','wmt_launcher'],virtual)}

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle',type=Path);parser.add_argument('output',type=Path)
    args=parser.parse_args();require(not args.output.exists(),'Audit output must be new')
    report=audit(args.bundle)
    with args.output.open('x') as destination:json.dump(report,destination,indent=2,sort_keys=True);destination.write('\n')
    args.output.chmod(0o600)
    print(f"WMT missing dependencies: {len(report['missing_required'])}; other missing: {len(report['missing_outside_wmt_closure'])}")
