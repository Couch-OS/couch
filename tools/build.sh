#!/bin/sh
# Build a bootable Linux image: stock kernel + our initramfs.
set -e
cd "$(dirname "$0")/.."
. tools/env.sh
mkdir -p build

[ -f build/kernel.bin ] || {
    echo "extracting the stock kernel from the backup..."
    [ -f "$BACKUP_DIR/boot.img" ] || { echo "no $BACKUP_DIR/boot.img - run tools/backup.sh"; exit 1; }
    python3 tools/bootimg.py unpack "$BACKUP_DIR/boot.img" build
}

python3 tools/build-busybox.py verify "${BUSYBOX_BUILD_DIR:-build/busybox-source}" --install build/busybox-armv7l

# On-screen console. This kernel has no CONFIG_VT, so without this a boot is silent.
tools/build-fbcon.sh

rm -rf build/tree
mkdir -p build/tree/extra
cp initramfs/init build/tree/init
cp initramfs/boot-health.sh build/tree/extra/boot-health.sh
cp build/busybox-armv7l build/tree/busybox
[ -f build/fbcon ] && cp build/fbcon build/tree/extra/fbcon
# Anything staged in initramfs/extra/ ships inside the image (modules, firmware).
[ -d initramfs/extra ] && cp -R initramfs/extra/. build/tree/extra/ 2>/dev/null || true

python3 tools/mkcpio.py build/tree build/initramfs.cpio
gzip -9 -c build/initramfs.cpio > build/initramfs.cpio.gz
python3 tools/bootimg.py pack build/kernel.bin build/initramfs.cpio.gz \
    build/linux-recovery.img "$BACKUP_DIR/boot.img"
