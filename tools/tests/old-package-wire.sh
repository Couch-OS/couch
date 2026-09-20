#!/usr/bin/env bash
# Do the packages already in the feed still work under this tree's host?
#
# The published Denon, Sonos and Kodi packages were built from couch-plugin and
# couch-sdk at 00ab4da. Their Request, Response, Envelope and Manifest refuse
# unknown fields, so one new byte in a frame makes such a child exit, and one
# new word in its answer has to be something this host still reads. The
# couch-plugin unit tests pin that with golden bytes and frozen copies of the
# old types; this runs the real thing: couch-plugin-echo and couch-plugin-sonos
# built from that source, driven by THIS tree's host through this tree's own
# admission and subprocess suites.
#
#   tools/tests/old-package-wire.sh
#   COUCH_OLD_WIRE_REV=<commit> tools/tests/...   # against another SDK revision
#   COUCH_OLD_WIRE_REUSE_TARGET=1 tools/tests/... # build this tree's tests in
#                                                 # clients/target (CI's cache)
#
# Needs git and cargo (and a C compiler: Sonos links ring). Fetches the commit
# if this clone lacks it. The old source is built in a temporary directory that
# is removed afterwards, and so are this tree's test binaries unless
# COUCH_OLD_WIRE_REUSE_TARGET is set; nothing else is written to the checkout.
# The Sonos developer key is not needed and must not be set: the old build has
# none baked in, exactly like a pull-request build.
set -euo pipefail

REV="${COUCH_OLD_WIRE_REV:-00ab4da2e6336a925e93702507c0b7b233012738}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/couch-old-wire.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
unset COUCH_SONOS_BUILT_IN_API_KEY

if ! git -C "$ROOT" cat-file -e "$REV^{commit}" 2>/dev/null; then
  echo "fetching $REV"
  git -C "$ROOT" fetch --quiet --depth 1 origin "$REV"
fi

mkdir -p "$WORK/old"
git -C "$ROOT" archive "$REV" clients model | tar -x -C "$WORK/old"

started=$SECONDS
echo "building couch-plugin-echo and couch-plugin-sonos from ${REV:0:7}"
CARGO_TARGET_DIR="$WORK/target-old" cargo build --quiet --locked \
  --manifest-path "$WORK/old/clients/Cargo.toml" \
  -p couch-echo -p couch-sonos --bin couch-plugin-echo --bin couch-plugin-sonos
echo "  built in $((SECONDS - started)) s"
echo_binary="$WORK/target-old/debug/couch-plugin-echo"
sonos_binary="$WORK/target-old/debug/couch-plugin-sonos"
test -x "$echo_binary" && test -x "$sonos_binary"

if [ -z "${COUCH_OLD_WIRE_REUSE_TARGET:-}" ]; then
  export CARGO_TARGET_DIR="$WORK/target-new"
fi
suite() { cargo test --locked --manifest-path "$ROOT/clients/Cargo.toml" "$@"; }

echo
echo "this tree's host and suites, the old echo package"
COUCH_ADMISSION_BINARY_ECHO="$echo_binary" suite -p couch-echo --test admission --test plugin

echo
echo "this tree's host and suites, the old Sonos package"
COUCH_ADMISSION_BINARY_SONOS="$sonos_binary" suite -p couch-sonos --test admission

# Two controls, so that a green run above means what it says.
echo
echo "control: the old child really does refuse a frame with a new field"
COUCH_ADMISSION_BINARY_ECHO="$echo_binary" suite -p couch-echo --test plugin -- \
  --ignored --exact control_a_child_built_from_the_published_sdk_exits_on_a_key_phase
COUCH_ADMISSION_BINARY_ECHO="$echo_binary" suite -p couch-echo --test plugin -- \
  --ignored --exact control_a_child_built_from_the_published_sdk_exits_on_a_child_of_a_connection

echo
echo "control: the suites really do run the executable they are given"
if COUCH_ADMISSION_BINARY_ECHO="$WORK/no-such-executable" \
  suite -p couch-echo --test admission -- --exact conformance >"$WORK/control.log" 2>&1; then
  echo "the echo admission suite passed without its executable: the override is not read"
  exit 1
fi
if COUCH_ADMISSION_BINARY_SONOS="$WORK/no-such-executable" \
  suite -p couch-sonos --test admission -- --exact conformance >"$WORK/control.log" 2>&1; then
  echo "the Sonos admission suite passed without its executable: the override is not read"
  exit 1
fi
echo "  both fail without it, as they must"

echo
echo "old package wire: every suite passed against packages built from ${REV:0:7} ($((SECONDS - started)) s)"
