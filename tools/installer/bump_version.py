#!/usr/bin/env python3
"""Set the next installer build version, independently of published OS releases."""
import argparse
from pathlib import Path
import re

VERSION_FILE = Path(__file__).resolve().with_name('VERSION')
VERSION = re.compile(r'v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?')


def validate(value):
    if not isinstance(value, str) or len(value) > 128 or not VERSION.fullmatch(value):
        raise ValueError('Expected an installer version such as v0.1.0 or v0.1.0-alpha.1')
    return value


def bump(value, path=VERSION_FILE):
    validate(value)
    path.write_text(value + '\n', encoding='utf-8', newline='\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('version', nargs='?')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.check:
        if args.version:
            parser.error('--check takes no version')
        print(validate(VERSION_FILE.read_text(encoding='utf-8').strip()))
    elif args.version:
        bump(args.version)
    else:
        parser.error('a version or --check is required')
