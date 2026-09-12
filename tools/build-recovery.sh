#!/bin/sh
# Build the recovery image: the STOCK kernel plus recovery/init.
#
# The kernel is deliberately the stock one rather than whatever is being worked
# on - recovery is the thing that has to boot when the experiment does not, so
# it must never share a kernel with the experiment. It is taken from the known
# good Couch image, which is the same vendor kernel and is proven on this board.
set -e
cd "$(dirname "$0")/.."
. tools/env.sh
mkdir -p build

KNOWN=${KNOWN:-build/linux-recovery-KNOWNGOOD.img}
[ -f "$KNOWN" ] || { echo "no $KNOWN - a known good Couch image is the kernel source"; exit 1; }
python3 - "$KNOWN" build/stock-kernel.bin <<'PY'
import struct, sys
d = open(sys.argv[1], "rb").read()
ks, _, _, _, _, _, _, pgsz = struct.unpack("<8I", d[8:40])
open(sys.argv[2], "wb").write(d[pgsz:pgsz + ks])
print("stock kernel:", ks, "bytes")
PY

tools/build-fbcon.sh
python3 tools/build-busybox.py verify "${BUSYBOX_BUILD_DIR:-build/busybox-source}" --install build/busybox-armv7l

rm -rf build/rec
mkdir -p build/rec/extra
cp recovery/init build/rec/init
cp build/busybox-armv7l build/rec/busybox
cp build/fbcon build/rec/extra/fbcon

python3 tools/mkcpio.py build/rec build/recovery.cpio
gzip -9 -c build/recovery.cpio > build/recovery.cpio.gz
python3 tools/bootimg.py pack build/stock-kernel.bin build/recovery.cpio.gz \
    build/couch-recovery.img "$BACKUP_DIR/boot.img"
echo "recovery -> build/couch-recovery.img"
echo "flash with: mtk.py w recovery build/couch-recovery.img   (p9 - keep android-p9-BACKUP.img)"
