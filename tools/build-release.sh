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
#
# Not everything built here goes to the same place. The runtime bundle carries
# only names the OLDEST DEPLOYED UPDATER accepts; the Bluetooth stack does not
# qualify, so it goes in the boot ramdisk's /extra instead
# (tools/release/prepare_boot_candidates.py). The two lists are
# runtime_inventory.RUNTIME and runtime_inventory.BOOT_EXTRA, the split is
# explained in tools/release/update_floor.py, and
# `python3 tools/release/update_floor.py --tree CLEAN_RUNTIME` checks a staged
# tree before it is signed.
#
# COUCH_PROTOCOL_3_PREVIEW=dev-remote-only builds couch-confd, and nothing else,
# with protocol 3 switched on, for one development remote
# (tools/protocol-3-preview-env.sh; docs/development/protocol.md, "The switch").
# Such a build may only be published as `...<N>.p3.dev`; the publisher refuses
# any other version for it.
set -eu
cd "$(dirname "$0")/.."
[ "$#" -eq 0 ] || {
    echo 'Usage: [COUCH_PROTOCOL_3_PREVIEW=dev-remote-only] tools/build-release.sh' >&2; exit 2; }
. tools/protocol-3-preview-env.sh
preview_banner
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
# The Bluetooth stack: boot ramdisk, not runtime bundle (see the header).
echo '= couch-bt-bridge (boot ramdisk /extra)'
(cd clients && cargo build --locked --release --target "$TARGET" -p couch-bt)
echo '= couch-bt-hid (boot ramdisk /extra)'
(cd clients && cargo build --locked --release --target "$TARGET" -p couch-bt-hid)
echo '= couch-bluetoothd (patched BlueZ, docker; boot ramdisk /extra)'
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

# The preview marker is in couch-confd exactly when this was a preview build,
# and in nothing else ever: no other binary has the switch, and a marked file
# under another name would get past the publisher, which reads couch-confd.
echo '= runtime bundle binaries (published in the signed update)'
for bin in ui/target/$TARGET/release/couch-gui \
    daemon/target/$TARGET/release/couch-confd \
    daemon/target/$TARGET/release/couch-system \
    clients/target/$TARGET/release/couch-sonos \
    clients/target/$TARGET/release/couch-coreelec; do
    case "$bin" in
        */couch-confd) check_preview_marker "$bin" "$PREVIEW" ;;
        *) check_preview_marker "$bin" no ;;
    esac
    printf '%s (%s bytes)\n' "$bin" "$(wc -c < "$bin" | tr -d ' ')"
done
echo '= boot ramdisk /extra binaries (never in the runtime bundle)'
for bin in clients/target/$TARGET/release/couch-bt-bridge \
    clients/target/$TARGET/release/couch-bt-hid \
    build/bluez/couch-bluetoothd; do
    check_preview_marker "$bin" no
    printf '%s (%s bytes)\n' "$bin" "$(wc -c < "$bin" | tr -d ' ')"
done
preview_banner
