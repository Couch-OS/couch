#!/bin/sh
# Static ARM build of keytap with the repository's Zig wrapper toolchain
# (tools/arm-cc-env.sh); run from the repository root on Ollie or any host
# with build/toolchains populated by tools/fetch-zig.sh. Output: build/keytap.
set -eu
cd "$(dirname "$0")/../.."
. tools/arm-cc-env.sh
mkdir -p build
"$ZIG" cc -target arm-linux-musleabihf -mcpu=cortex_a7 -static -Os -Wall -Wextra \
    -o build/keytap tools/keypad/keytap.c
ls -l build/keytap
