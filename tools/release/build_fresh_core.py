#!/usr/bin/env python3
"""Build frozen public OS runtime inputs and retain their original build receipt."""
import argparse
import json
from pathlib import Path
import re
import subprocess

from clean_stage import REPO, checksum, require
from fresh_os import CORE
from runtime_inventory import arm_static, regular


def frozen_source(root):
    commit = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
    require(re.fullmatch('[0-9a-f]{40}', commit), 'Expected a frozen source commit')
    dirty = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain=v1',
                                     '--untracked-files=all'], text=True)
    require(not dirty, 'Fresh OS release build requires a clean frozen source checkout')
    return commit


def build(output, root=REPO):
    require(not output.exists() and not output.is_symlink(), 'Core build receipt output must be new')
    commit = frozen_source(root)
    # These are the maintained full-OS recipes. Do not synthesize a build receipt
    # by hashing previously produced executables in a newer checkout.
    for script in ('tools/build-wmt-properties.sh', 'tools/build-release.sh'):
        subprocess.run([str(root / script)], cwd=root, check=True)
    require(frozen_source(root) == commit, 'Source changed during fresh core build')
    files = []
    key = regular(root / 'daemon/couch-integrations/src/official.rsa.pub')
    for name, source in sorted(CORE.items()):
        content = regular(root / source)
        arm_static(content)
        if name == 'couch-confd':
            require(key in content, 'Built core does not embed official integration key')
        files.append(dict(path=source, size=len(content), sha256=checksum(content)))
    result = dict(schema=1, kind='couch-unsigned-runtime-build', installable=False,
                  source_commit=commit, target='armv7-unknown-linux-musleabihf', files=files)
    with output.open('x') as stream:
        stream.write(json.dumps(result, sort_keys=True, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    build(args.output)
