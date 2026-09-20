#!/bin/sh
# The config UI, and the daemon that carries it.
#
# The order is the whole reason this is a script. couch-confd's build.rs bakes
# web/couch-web/dist into the binary, so trunk has to have run first or the
# daemon ships the placeholder page that says it has no UI in it. Cargo cannot
# express that dependency - the two live in different workspaces, for different
# target triples, and only one of them is even built by cargo alone.
#
# The device target is the default because that is the artefact that matters;
# --host builds the same thing for this machine, which is what
# tools/run-webui.sh serves.
set -e
cd "$(dirname "$0")/.."
# The daemon links couch-sonos, so it carries the developer key too.
. tools/sonos-build-env.sh
# COUCH_PROTOCOL_3_PREVIEW=dev-remote-only builds the daemon with protocol 3
# switched on; every build, of either kind, is checked for the marker below.
. tools/protocol-3-preview-env.sh
preview_banner

TARGET=${TARGET:-armv7-unknown-linux-musleabihf}
[ "$1" = "--host" ] && TARGET=host

command -v trunk >/dev/null || {
    echo "no trunk on PATH. It builds the wasm bundle:"
    echo "  cargo install --locked trunk"
    echo "  rustup target add wasm32-unknown-unknown"
    exit 1
}

echo "= web/couch-web -> dist"
( cd web/couch-web && trunk build --release )
# couch-confd's build.rs reruns when this changes (see the comment there).
COUCH_WEB_DIST_DIGEST=$(cd web/couch-web/dist && find . -type f | LC_ALL=C sort |
    while read -r f; do sha256sum "$f" 2>/dev/null || shasum -a 256 "$f"; done |
    { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1)
export COUCH_WEB_DIST_DIGEST

# An ordinary build is the whole daemon workspace with default features. A
# preview build is couch-confd alone with the one feature that is the switch,
# in the form CI uses (.github/workflows/integration-admission.yml); `-p` is
# what lets a workspace build name another package's feature. Cargo keys its
# artefacts on the feature set, so going from one kind of build to the other
# relinks the daemon, and check_preview_marker below proves it each time.
CONFD_ONLY_WITH_PROTOCOL_3=
[ "$PREVIEW" = no ] ||
    CONFD_ONLY_WITH_PROTOCOL_3='-p couch-confd --features couch-plugin/protocol-3-preview'
if [ "$TARGET" = host ]; then
    echo "= daemon/couch-confd -> host"
    ( cd daemon && cargo build --release $CONFD_ONLY_WITH_PROTOCOL_3 )
    BIN=daemon/target/release/couch-confd
else
    # rust-lld links the final binary; ring's C needs the ARM musl compiler
    # that tools/arm-cc-env.sh provides on macOS and Linux alike.
    . tools/arm-cc-env.sh
    rustup target list --installed | grep -qx "$TARGET" || {
        echo "no $TARGET toolchain: rustup target add $TARGET"; exit 1; }
    echo "= daemon/couch-confd -> $TARGET"
    ( cd daemon && cargo build --release --target "$TARGET" $CONFD_ONLY_WITH_PROTOCOL_3 )
    BIN=daemon/target/$TARGET/release/couch-confd
fi

# The daemon must carry the UI that was just built, not one a stale build
# script cache baked in. trunk names its bundle by content hash and the asset
# table stores those names, so their presence in the binary is the proof.
for f in web/couch-web/dist/couch-web-*; do
    [ -f "$f" ] || { echo "no browser bundle in web/couch-web/dist" >&2; exit 1; }
    grep -a -q -F "$(basename "$f")" "$BIN" || {
        echo "$BIN does not contain $(basename "$f"): it embeds a stale web UI." >&2
        echo "Remove daemon/target/*/release/build/couch-confd-* and rebuild." >&2
        exit 1
    }
done

# Protocol 3 is on in this daemon exactly when it was asked for.
check_preview_marker "$BIN" "$PREVIEW"

# The sizes are the point of the exercise: everything here is downloaded over
# the remote's own WiFi or stored on its flash, so a build that quietly doubled
# is worth seeing at the end of every run.
echo
for f in web/couch-web/dist/*; do
    [ -f "$f" ] || continue
    printf "  %-46s %7s  %7s gz\n" "$(basename "$f")" \
        "$(wc -c < "$f" | tr -d ' ')" "$(gzip -9 -c "$f" | wc -c | tr -d ' ')"
done
printf "  %-46s %7s\n" "couch-confd ($TARGET)" "$(wc -c < "$BIN" | tr -d ' ')"
echo "  $BIN"
preview_banner
