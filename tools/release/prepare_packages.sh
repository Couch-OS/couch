#!/bin/sh
# Run inside a digest-pinned Alpine container; never runs ARM programs/scripts.
set -eu
OUT=/out
APK_ARCH=${APK_ARCH:-armv7}
case "$APK_ARCH" in armv7|x86_64) ;; *) exit 2 ;; esac
MIRROR=https://dl-cdn.alpinelinux.org/alpine/v3.21
mkdir "$OUT/packages" "$OUT/indexes" "$OUT/keys"
cp /usr/share/apk/keys/"$APK_ARCH"/*.pub "$OUT/keys/"
printf '%s\n' "$MIRROR/main" "$MIRROR/community" > /tmp/repositories
cp /tmp/repositories "$OUT/repositories"
apk --version > "$OUT/apk-version.txt"
# A separate root prevents the x86 builder's installed packages satisfying ARM
# dependencies. Keep signed indexes and keys as reviewable provenance inputs.
mkdir -p /tmp/resolver/etc/apk /tmp/resolver/lib/apk/db
touch /tmp/resolver/lib/apk/db/installed
# apk-tools 2.14 fetch treats positional arguments as literal package names.
# WORLD is its dependency-constraint interface: retain exact name=version roots
# for the solver instead of silently fetching the latest unpinned package.
printf '%s\n' "$@" > /tmp/resolver/etc/apk/world
apk --root /tmp/resolver --arch "$APK_ARCH" --keys-dir "$OUT/keys" \
    --repositories-file /tmp/repositories --cache-dir "$OUT/indexes" update
apk --root /tmp/resolver --arch "$APK_ARCH" --keys-dir "$OUT/keys" \
    --repositories-file /tmp/repositories --cache-dir "$OUT/indexes" \
    fetch --world --recursive --simulate --url > "$OUT/package-urls.txt"
apk --root /tmp/resolver --arch "$APK_ARCH" --keys-dir "$OUT/keys" \
    --repositories-file /tmp/repositories --cache-dir "$OUT/indexes" \
    fetch --world --recursive --output "$OUT/packages"
sh /check.sh > "$OUT/offline-solve.txt"
