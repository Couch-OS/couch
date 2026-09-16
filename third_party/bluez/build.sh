#!/bin/sh
# Build couch-bluetoothd: Alpine 3.21's bluetoothd (bluez 5.79-r0) with the
# Couch patches in this directory, for the HA100 (armv7, musl, linked against
# the Alpine libraries the remote already has). It is a drop-in for the
# remote's /usr/lib/bluetooth/bluetoothd: same configure paths (/etc/bluetooth,
# /var/lib/bluetooth), same flags as the aports recipe, same shared libraries.
# Why it exists: README.md.
#
# Usage: third_party/bluez/build.sh OUTDIR
#
# Needs docker with linux/arm/v7 emulation (binfmt qemu-arm) and, on the first
# run, network access. Everything that decides the bytes is pinned below: the
# upstream tarball by SHA-256, the aports recipe by commit (its patches by the
# recipe's own SHA-512 sums), the build container by digest and the libraries
# and compiler by package version. A pinned package that has left the 3.21
# repository fails the build instead of silently changing it.
#
# OUTDIR afterwards (OUTDIR must not exist):
#   couch-bluetoothd   the stripped binary
#   build.json         inputs, container, package versions, binary SHA-256
#   packages.txt       every package installed in the build container
#   source/            the corresponding source: upstream tarball, the aports
#                      recipe files, the Couch patches, this script, README.md
#                      (tools/release/collect_external_sources.py bluez turns
#                      it into a release source component)
set -eu

BLUEZ_VERSION=5.79
BLUEZ_TARBALL=bluez-$BLUEZ_VERSION.tar.xz
BLUEZ_URL=https://www.kernel.org/pub/linux/bluetooth/$BLUEZ_TARBALL
BLUEZ_SHA256=4164a5303a9f71c70f48c03ff60be34231b568d93a9ad5e79928d34e6aa0ea8a
# aports 3.21-stable; main/bluez is 5.79-r0 there, the package the remote runs.
APORTS_COMMIT=36c5dde037fc6e07b8e2f3197463422ccc963f11
APORTS_RAW=https://gitlab.alpinelinux.org/alpine/aports/-/raw/$APORTS_COMMIT/main/bluez
# docker.io/library/alpine:3.21 (3.21.7), the linux/arm/v7 image manifest.
IMAGE=alpine@sha256:e2d6b24023ccaac17dba05f7f085b89bd3a29eb4ee029f7ba71f5a0652ea0067
# The libraries bluetoothd links, at the versions on the remote, and the
# toolchain that produced the published binary.
PINNED="glib-dev=2.82.5-r0 dbus-dev=1.14.10-r4 eudev-dev=3.2.14-r5 musl-dev=1.2.5-r11 gcc=14.2.0-r4 binutils=2.43.1-r3"

HERE=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 1 ] || { echo 'Usage: third_party/bluez/build.sh OUTDIR' >&2; exit 2; }
[ ! -e "$1" ] || { echo "$1 exists; use a fresh output directory" >&2; exit 2; }
mkdir -p "$1"
OUT=$(cd "$1" && pwd)
CACHE=${COUCH_BLUEZ_CACHE:-$HOME/.cache/couch-bluez}
mkdir -p "$CACHE" "$OUT/source/aports"

fetch() { # URL FILE
    [ -s "$2" ] || { curl -fsSL "$1" -o "$2.part" && mv "$2.part" "$2"; }
}

fetch "$BLUEZ_URL" "$CACHE/$BLUEZ_TARBALL"
echo "$BLUEZ_SHA256  $CACHE/$BLUEZ_TARBALL" | sha256sum -c -
cp "$CACHE/$BLUEZ_TARBALL" "$OUT/source/"

fetch "$APORTS_RAW/APKBUILD" "$CACHE/APKBUILD-$APORTS_COMMIT"
cp "$CACHE/APKBUILD-$APORTS_COMMIT" "$OUT/source/aports/APKBUILD"
# Every local file the recipe names, checked against its sha512sums.
for name in $(sed -n '/^source="/,/"/p' "$OUT/source/aports/APKBUILD" |
        tr -d '"' | sed 's/^source=//' | grep -v '://' | grep .); do
    fetch "$APORTS_RAW/$name" "$CACHE/$APORTS_COMMIT-$name"
    cp "$CACHE/$APORTS_COMMIT-$name" "$OUT/source/aports/$name"
done
(cd "$OUT/source/aports" &&
    sed -n '/^sha512sums="/,/"/p' APKBUILD | tr -d '"' | sed 's/^sha512sums=//' |
    grep -v " $BLUEZ_TARBALL\$" | grep . | sha512sum -c -)

cp "$HERE"/0*.patch "$HERE/build.sh" "$HERE/README.md" "$OUT/source/"

docker run --rm --platform linux/arm/v7 \
    -e PINNED="$PINNED" -e BLUEZ_TARBALL="$BLUEZ_TARBALL" -e OWNER="$(id -u):$(id -g)" \
    -v "$OUT:/out" "$IMAGE" sh -euc '
apk add --no-cache build-base abuild patch $PINNED >/dev/null
cd /tmp
cp /out/source/aports/APKBUILD .
# The recipe is shell; read its dependency and source lists from it.
makedepends=$(. ./APKBUILD; echo $makedepends)
patches=$(. ./APKBUILD; for s in $source; do case $s in *.patch) echo "${s##*/}";; esac; done)
apk add --no-cache $makedepends $PINNED >/dev/null
apk info -v | sort > /out/packages.txt
# abuild'"'"'s own build environment (CFLAGS, LDFLAGS).
. /usr/share/abuild/default.conf
. /etc/abuild.conf
tar xf /out/source/$BLUEZ_TARBALL
cd bluez-*
for p in $patches; do patch -p1 -i /out/source/aports/$p; done
for p in /out/source/0*.patch; do patch -p1 -i "$p"; done
# The configure line and CFLAGS of the recipe'"'"'s build().
CHOST=armv7-alpine-linux-musleabihf
CFLAGS="$CFLAGS -flto=auto" ./configure \
    --build=$CHOST --host=$CHOST --prefix=/usr --sysconfdir=/etc \
    --mandir=/usr/share/man --localstatedir=/var --libexecdir=/usr/lib \
    --disable-systemd --enable-library --enable-deprecated --enable-hid2hci \
    --enable-mesh --enable-sixaxis --with-dbusconfdir=/usr/share >/tmp/configure.log 2>&1 ||
    { tail -40 /tmp/configure.log; exit 1; }
# Only bluetoothd: its generated headers first (a plain target does not build
# BUILT_SOURCES), then the daemon.
built=$(printf "print-built:\n\t@echo \$(BUILT_SOURCES)\n" | make -s -f Makefile -f - print-built)
{ make -j"$(nproc)" $built && make -j"$(nproc)" src/bluetoothd; } >/tmp/make.log 2>&1 ||
    { grep -B2 -A8 -E "error|Error" /tmp/make.log | tail -60; exit 1; }
grep -E "warning:" /tmp/make.log | grep -E "gatt-database|device\.c" || true
strip -o /out/couch-bluetoothd src/bluetoothd
scanelf -qF "%n#F" /out/couch-bluetoothd | tr "," "\n" > /out/needed.txt
printf "%s\n" "$CFLAGS -flto=auto" > /out/cflags.txt
printf "%s\n" "$LDFLAGS" > /out/ldflags.txt
printf "%s\n" $patches > /out/aports-patches.txt
chown "$OWNER" /out/couch-bluetoothd /out/*.txt
'

SHA=$(sha256sum "$OUT/couch-bluetoothd" | cut -d' ' -f1)
python3 - "$OUT" "$SHA" <<EOF
import hashlib, json, pathlib, sys
out, binary = pathlib.Path(sys.argv[1]), sys.argv[2]
def lines(name): return (out / name).read_text().split('\n')[:-1]
source = sorted(p.relative_to(out / 'source').as_posix() for p in (out / 'source').rglob('*') if p.is_file())
receipt = {
    'schema': 1, 'kind': 'couch-bluez-build', 'binary': 'couch-bluetoothd', 'binary_sha256': binary,
    'bluez_version': '$BLUEZ_VERSION', 'bluez_url': '$BLUEZ_URL', 'bluez_sha256': '$BLUEZ_SHA256',
    'aports_commit': '$APORTS_COMMIT', 'aports_recipe': 'main/bluez', 'aports_patches': lines('aports-patches.txt'),
    'couch_patches': sorted(p.name for p in (out / 'source').glob('0*.patch')),
    'container': '$IMAGE', 'platform': 'linux/arm/v7', 'pinned_packages': '$PINNED'.split(),
    'cflags': lines('cflags.txt')[0], 'ldflags': lines('ldflags.txt')[0], 'needed': lines('needed.txt'),
    'packages': lines('packages.txt'),
    'source_files': {name: hashlib.sha256((out / 'source' / name).read_bytes()).hexdigest() for name in source},
}
(out / 'build.json').write_text(json.dumps(receipt, indent=2) + '\n')
EOF
rm -f "$OUT/cflags.txt" "$OUT/ldflags.txt" "$OUT/needed.txt" "$OUT/aports-patches.txt"
echo "couch-bluetoothd $SHA"
