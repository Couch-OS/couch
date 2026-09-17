#!/usr/bin/env python3
"""Compatibility entry point; native launcher implementation lives in tools/installer."""
import importlib.util
from pathlib import Path
import sys

_DIRECTORY = Path(__file__).resolve().parents[1] / 'installer'
sys.path.insert(0, str(_DIRECTORY))
_SPEC = importlib.util.spec_from_file_location('_couch_installer_launchers', _DIRECTORY / 'installer_launchers.py')
_MODULE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_MODULE)
PLATFORMS = _MODULE.PLATFORMS
generate = _MODULE.generate

if __name__ == '__main__':
    _MODULE.main()
