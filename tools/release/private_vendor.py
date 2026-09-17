#!/usr/bin/env python3
"""Compatibility entry point for the installer-owned vendor helper."""
from pathlib import Path
import runpy
import sys

from installer_pins import PINS, load

if __name__ == '__main__':
    sys.path.insert(0, str(PINS))
    runpy.run_path(str(PINS / 'private_vendor.py'), run_name='__main__')
else:
    _impl = load('private_vendor')
    globals().update({name: value for name, value in vars(_impl).items() if not name.startswith('_')})
