#!/usr/bin/env python3
"""Compatibility entry point for the installer-owned neutral RAM builder."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'installer/image'))
from neutral_ramdisk import main, prepare


if __name__ == '__main__':
    main()
