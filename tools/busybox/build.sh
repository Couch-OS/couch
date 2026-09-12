#!/bin/sh
# Runs in the pinned ARMv7 Alpine builder with networking disabled.
set -eu
apk add --no-network --no-cache /cache/packages/*.apk > /out/toolchain-install.log
cd /out
tar -xjf busybox-source.tar.bz2
cd busybox-1.37.0
cp /recipe/busybox.config .config
export KCONFIG_NOTIMESTAMP=1 SOURCE_DATE_EPOCH=1725494400
make oldconfig </dev/null > /out/configure.log
cmp .config /recipe/busybox.config
make -j4 EXTRA_CFLAGS='-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard' > /out/compile.log 2>&1
cp busybox /out/busybox-armv7l
./busybox --list > /out/applets.txt
gcc -v > /out/compiler.txt 2>&1
apk info -v > /out/installed-packages.txt
readelf -h -l -A busybox > /out/elf.txt
/out/busybox-armv7l sh /recipe/smoke.sh > /out/smoke.txt 2>&1
