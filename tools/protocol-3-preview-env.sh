# Sourced by tools/build-release.sh and tools/build-webui.sh from the
# repository root: the one opt-in for a build with protocol 3 switched on
# (docs/development/protocol.md, "The switch").
#
#     COUCH_PROTOCOL_3_PREVIEW=dev-remote-only tools/build-release.sh
#
# The value has to be spelt out in full. Anything else that is not empty is a
# usage error and not "off": a typo must not quietly produce an ordinary build
# that somebody then believes is the preview, or the other way round.
#
# Only couch-confd is built with the feature. It writes a marker line into its
# own bytes when, and only when, it admits protocol 3
# (daemon/couch-confd/src/assets.rs). A Cargo feature is invisible in a source
# tree, a file list and a version string, so that line is what everything
# downstream looks for: check_preview_marker here after every build,
# tools/release/runtime_inventory.py, tools/release/update_floor.py, and
# couch_updates::bundle, which refuses to sign a marked daemon under any
# version that is not a `.p3.dev` one.
PREVIEW_MARKER='COUCH-PREVIEW-BUILD protocol-3'
case "${COUCH_PROTOCOL_3_PREVIEW:-}" in
    '') PREVIEW=no ;;
    dev-remote-only) PREVIEW=yes ;;
    *)
        echo "COUCH_PROTOCOL_3_PREVIEW=$COUCH_PROTOCOL_3_PREVIEW is not a value this build knows." >&2
        echo 'Unset it for an ordinary build, or set it to exactly dev-remote-only for a' >&2
        echo 'protocol 3 preview build (docs/development/protocol.md, "The switch").' >&2
        exit 2 ;;
esac
export COUCH_PROTOCOL_3_PREVIEW="${COUCH_PROTOCOL_3_PREVIEW:-}"

# Printed first and last, to stderr so a piped or tailed log still shows it.
preview_banner() {
    [ "$PREVIEW" = yes ] || return 0
    {
        echo '#####################################################################'
        echo '# PROTOCOL 3 PREVIEW BUILD - development remote only'
        echo '# The tag must end .p3.dev - never Alpha, never any other remote.'
        echo '# couch-updates refuses to sign this couch-confd under any other tag.'
        echo '#####################################################################'
    } >&2
}

# check_preview_marker BINARY WANTED(yes|no): the marker is in BINARY exactly
# when WANTED says so. Run after every build, preview or not. It catches a
# preview daemon left in the target directory by the other kind of build as
# surely as it catches a preview build whose marker the linker dropped.
check_preview_marker() {
    [ -f "$1" ] || { echo "$1: not built" >&2; exit 1; }
    if grep -a -q -F "$PREVIEW_MARKER" "$1"; then
        [ "$2" = yes ] && return 0
        echo "$1 carries the protocol 3 preview marker, and this is not a preview build." >&2
        echo 'It was left by a preview build, or something switched the feature on.' >&2
        echo 'Delete that file and rebuild; do not stage or sign it.' >&2
        exit 1
    fi
    [ "$2" = no ] && return 0
    echo "$1 has no protocol 3 preview marker, and a preview build was asked for." >&2
    echo 'The feature did not reach couch-confd, or the linker dropped the marker' >&2
    echo '(daemon/couch-confd/src/assets.rs). Without it nothing downstream can tell' >&2
    echo 'this daemon from an ordinary one, so it must not be staged or signed.' >&2
    exit 1
}
