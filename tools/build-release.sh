#!/bin/sh
# Build every ARM binary the runtime payload inventory lists, on the release
# host, in one command (couch-bluetoothd also needs docker with arm/v7
# emulation). Each step is the same script or cargo invocation a
# developer runs by hand; this file only fixes the order and makes sure the
# two pieces of environment that cargo cannot supply - the ARM musl C compiler
# for ring, and the Sonos developer key - are set once for all of them.
#
# Afterwards, run tools/release/runtime_inventory.py on this same checkout
# (docs/runtime-payload.md) so the hashes describe the binaries just built.
set -eu
cd "$(dirname "$0")/.."
[ "$#" -eq 0 ] || { echo 'Usage: tools/build-release.sh' >&2; exit 2; }
TARGET=armv7-unknown-linux-musleabihf
rustup target list --installed | grep -qx "$TARGET" || {
    echo "no $TARGET toolchain: rustup target add $TARGET" >&2; exit 1; }
. tools/arm-cc-env.sh
. tools/sonos-build-env.sh

echo '= couch-gui'
tools/build-gui.sh
echo '= couch-confd (with the browser bundle)'
tools/build-webui.sh
echo '= couch-system'
(cd daemon && cargo build --locked --release --target "$TARGET" -p couch-system)
echo '= couch-sonos'
tools/build-sonos.sh
echo '= couch-coreelec'
(cd clients && cargo build --locked --release --target "$TARGET" -p couch-coreelec)
echo '= couch-bt-bridge'
(cd clients && cargo build --locked --release --target "$TARGET" -p couch-bt)
echo '= couch-bt-hid'
(cd clients && cargo build --locked --release --target "$TARGET" -p couch-bt-hid)
echo '= couch-bluetoothd (patched BlueZ, docker)'
# Rebuilt only when the patch, the recipe or its README changed since the
# last build: under emulation it takes a few minutes (third_party/bluez).
bluez_current() {
    [ -f build/bluez/build.json ] && [ -f build/bluez/couch-bluetoothd ] || return 1
    for f in third_party/bluez/0*.patch third_party/bluez/build.sh third_party/bluez/README.md; do
        grep -q "\"${f##*/}\": \"$(sha256sum "$f" | cut -d' ' -f1)\"" build/bluez/build.json || return 1
    done
    [ "$(ls third_party/bluez/0*.patch | wc -l)" -eq "$(grep -c '^    "0.*\.patch": ' build/bluez/build.json)" ]
}
if ! bluez_current; then
    rm -rf build/bluez
    third_party/bluez/build.sh build/bluez
fi

echo '= release binaries'
for bin in ui/target/$TARGET/release/couch-gui \
    daemon/target/$TARGET/release/couch-confd \
    daemon/target/$TARGET/release/couch-system \
    clients/target/$TARGET/release/couch-sonos \
    clients/target/$TARGET/release/couch-coreelec \
    clients/target/$TARGET/release/couch-bt-bridge \
    clients/target/$TARGET/release/couch-bt-hid \
    build/bluez/couch-bluetoothd; do
    printf '%s (%s bytes)\n' "$bin" "$(wc -c < "$bin" | tr -d ' ')"
done
