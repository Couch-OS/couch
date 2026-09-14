#!/bin/sh
# Build every ARM binary the runtime payload inventory lists, on the release
# host, in one command. Each step is the same script or cargo invocation a
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

echo '= release binaries'
for bin in ui/target/$TARGET/release/couch-gui \
    daemon/target/$TARGET/release/couch-confd \
    daemon/target/$TARGET/release/couch-system \
    clients/target/$TARGET/release/couch-sonos \
    clients/target/$TARGET/release/couch-coreelec \
    clients/target/$TARGET/release/couch-bt-bridge \
    clients/target/$TARGET/release/couch-bt-hid; do
    printf '%s (%s bytes)\n' "$bin" "$(wc -c < "$bin" | tr -d ' ')"
done
