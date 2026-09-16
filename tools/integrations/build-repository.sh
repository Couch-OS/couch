#!/bin/sh
# Sign a small, explicit ARMv7 APK repository from integration APKs.
set -eu
[ "$#" = 3 ] || { echo "Usage: $0 PRIVATE_KEY APK_DIR OUTPUT_DIR" >&2; exit 64; }
key=$1
packages=$2
output=$3
[ -f "$key" ] && [ -f "$key.pub" ] && [ -d "$packages" ] || { echo "key, matching public key, or package directory is missing" >&2; exit 1; }
command -v apk >/dev/null && command -v abuild-sign >/dev/null || { echo "apk and abuild-sign are required" >&2; exit 1; }
mkdir -p "$output/armv7"
output="$output/armv7"
find "$packages" -type f -name '*.apk' -exec cp {} "$output/" \;
set -- "$output"/*.apk
[ -f "$1" ] || { echo "no APKs to index" >&2; exit 1; }
keydir=$(mktemp -d "${TMPDIR:-/tmp}/couch-integration-keys.XXXXXX")
trap 'rm -rf "$keydir"' EXIT HUP INT TERM
cp "$key.pub" "$keydir/"
apk --keys-dir "$keydir" index --rewrite-arch armv7 --output "$output/APKINDEX.tar.gz" "$@"
abuild-sign -k "$key" "$output/APKINDEX.tar.gz"
echo "repository: $output"
