#!/bin/sh
# Run with the freshly built ARM shell. No mounts, devices or network access.
set -eu
BB=/out/busybox-armv7l
WORK=$($BB mktemp -d /out/smoke.XXXXXX)
trap '$BB rm -rf "$WORK"' EXIT HUP INT TERM
[ "$($BB printf abc | $BB sha256sum | $BB cut -d ' ' -f1)" = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad ]
$BB timeout 2 "$BB" true
if $BB timeout 1 "$BB" sleep 5; then
    echo 'timeout failed to stop a child' >&2
    exit 1
fi
exec 9>"$WORK/lock"
$BB flock -n 9
if $BB flock -n "$WORK/lock" "$BB" true; then
    echo 'flock admitted another writer' >&2
    exit 1
fi
$BB flock -u 9
$BB flock -n "$WORK/lock" "$BB" true
$BB printf 'boot fixture\n' > "$WORK/input"
$BB gzip -c "$WORK/input" | $BB gzip -d > "$WORK/output"
$BB cmp "$WORK/input" "$WORK/output"
echo 'ARM shell, hashing, gzip, timeout and exclusive-lock checks passed.'
