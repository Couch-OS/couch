#!/usr/bin/env python3
"""Compatibility entry point for the installer-owned ARM musl compiler wrapper."""
from pathlib import Path
import runpy

runpy.run_path(str(Path(__file__).resolve().parent / 'installer/toolchain/arm-musl-cc.py'),
              run_name='__main__')
