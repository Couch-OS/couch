#!/usr/bin/env python3
"""Build the neutral installer RAM image with the pinned installer's builder."""
from installer_pins import load_neutral_ramdisk

_builder = load_neutral_ramdisk()
main, prepare = _builder.main, _builder.prepare


if __name__ == '__main__':
    main()
