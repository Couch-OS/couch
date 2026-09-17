#!/usr/bin/env python3
"""Compatibility entry point for installer-owned official input preparation."""
from pathlib import Path
import runpy
import sys

from installer_pins import PINS, load

if __name__ == '__main__':
    sys.path.insert(0, str(PINS))
    runpy.run_path(str(PINS / 'prepare_official_inputs.py'), run_name='__main__')
else:
    _impl = load('prepare_official_inputs')
    globals().update({name: value for name, value in vars(_impl).items() if not name.startswith('_')})
