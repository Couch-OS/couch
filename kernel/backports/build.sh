#!/bin/sh
# Build the backported Bluetooth core (backports-4.4.2-1) plus Couch's hci_stp
# driver as modules for a kernel output tree, inside the pinned couch-kbuild
# container. See docs/kernel-backports-research.md.
#
#   kernel/backports/build.sh KTREE KOUT [BACKPORTS_DIR]
#
# KTREE: the kernel source tree the output was built from (must sit at the
# commit the output records); KOUT: the kernel output directory built with
# CONFIG_BT off and CONFIG_CRYPTO_CMAC on. Produces compat.ko, bluetooth.ko,
# hci_vhci.ko and hci_stp.ko, stripped of debug info, in $KOUT/backports/.
set -eu
KTREE=$1; KOUT=$2; BP=${3:-$HOME/backports/backports-4.4.2-1}
HERE=$(cd "$(dirname "$0")" && pwd)
TARBALL=backports-4.4.2-1.tar.xz
URL=https://cdn.kernel.org/pub/linux/kernel/projects/backports/stable/v4.4.2/$TARBALL
if [ ! -d "$BP" ]; then
    mkdir -p "$(dirname "$BP")"
    (cd "$(dirname "$BP")" && curl -sfO "$URL" && tar xf "$TARBALL")
fi
# The one shim patch 3.18 needs, and our driver as an extra backports module.
if ! grep -q "LINUX_VERSION_CODE < KERNEL_VERSION(3,8,0)" "$BP/backport-include/linux/cred.h"; then
    patch -d "$BP" -p0 < "$HERE/0001-cred-shim-3.18.patch"
fi
cp "$HERE/hci_stp.c" "$BP/drivers/bluetooth/hci_stp.c"
cp "$HERE/defconfig-bluetooth" "$BP/defconfigs/bluetooth"
grep -q BT_HCISTP "$BP/drivers/bluetooth/Kconfig" || cat >> "$BP/drivers/bluetooth/Kconfig" <<'K'

config BT_HCISTP
	tristate "MediaTek STP HCI driver (Couch)"
	depends on m
	depends on BT
	help
	  BlueZ HCI driver over the MediaTek combo STP transport, for the
	  Sanytron HA100 (Couch). The transport symbols come from the base
	  kernel's connectivity driver.
K
grep -q hci_stp "$BP/drivers/bluetooth/Makefile" || cat >> "$BP/drivers/bluetooth/Makefile" <<'M'

obj-$(CPTCFG_BT_HCISTP)	+= hci_stp.o
CFLAGS_hci_stp.o += -I/src/drivers/misc/mediatek/connectivity/common/conn_soc/include -I/src/drivers/misc/mediatek/connectivity/common/conn_soc/linux/include -I/src/drivers/misc/mediatek/connectivity/common/conn_soc/core/include -I/src/drivers/misc/mediatek/connectivity/common/common_detect -I/src/drivers/misc/mediatek/include/mt-plat -I/src/drivers/misc/mediatek/include/mt-plat/mt6580/include/mach
M
KIMAGE=$(docker image inspect couch-kbuild --format '{{.Id}}')
docker run --rm --user "$(id -u):$(id -g)" -v "$KTREE":/src:ro -v "$KOUT":/out -v "$BP":/bp -w /bp "$KIMAGE" sh -ec '
    make KLIB_BUILD=/out ARCH=arm CROSS_COMPILE=arm-eabi- clean >/dev/null 2>&1 || true
    make KLIB_BUILD=/out ARCH=arm CROSS_COMPILE=arm-eabi- defconfig-bluetooth >/dev/null
    make KLIB_BUILD=/out ARCH=arm CROSS_COMPILE=arm-eabi- -j"$(nproc)" > /bp/build.log 2>&1
    mkdir -p /out/backports
    for m in compat/compat.ko net/bluetooth/bluetooth.ko drivers/bluetooth/hci_vhci.ko drivers/bluetooth/hci_stp.ko; do
        arm-eabi-strip --strip-debug "$m"; cp "$m" /out/backports/
    done
    arm-eabi-readelf -p .modinfo /out/backports/hci_stp.ko | grep -E "vermagic|depends"'
ls -la "$KOUT/backports"
