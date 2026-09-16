#!/usr/bin/env python3
"""Reject normal builds missing the promoted HA100 board initialization fixes."""
import json
from pathlib import Path
import subprocess
import sys


def check(tree, pin):
    commit = pin['source_commit']
    if subprocess.call(['git', '-C', str(tree), 'merge-base', '--is-ancestor', commit, 'HEAD']):
        raise ValueError(f'Normal kernel source must include promoted board fixes {commit}; update the configured source tree')


if __name__ == '__main__':
    check(sys.argv[1], json.loads(Path(__file__).with_name('release-pin.json').read_text()))
