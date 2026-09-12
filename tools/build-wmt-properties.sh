#!/bin/sh
# No libc is linked: the pinned Android launcher provides its Bionic symbols.
set -eu
cd "$(dirname "$0")/.."
mkdir -p build
${WMT_CC:-arm-linux-gnueabihf-gcc} -shared -fPIC -Os -nostdlib -fno-stack-protector \
    -Wl,--hash-style=sysv -o build/couch-wmt-properties.so \
    tools/installer/wifi-stage/wmt-properties.c
