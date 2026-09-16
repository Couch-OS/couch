#!/bin/sh
# Build one signed, architecture-labelled integration APK from a prebuilt
# static plugin binary. Run inside Alpine with abuild installed; this script
# does not download sources or run the integration binary.
set -eu

usage() {
    echo "Usage: $0 ID VERSION BINARY MANIFEST PRIVATE_KEY OUTPUT_DIR" >&2
    exit 64
}
[ "$#" = 6 ] || usage
id=$1
version=$2
binary=$3
manifest=$4
key=$5
output=$6
case "$id" in *[!a-z0-9_-]*|'') usage;; esac
case "$version" in *[!A-Za-z0-9._+-]*|'') usage;; esac
[ -f "$binary" ] && [ -f "$manifest" ] && [ -f "$key" ] || { echo "input is missing" >&2; exit 1; }
absolute() { cd "$(dirname "$1")" && printf '%s/%s\n' "$(pwd -P)" "$(basename "$1")"; }
binary=$(absolute "$binary")
manifest=$(absolute "$manifest")
key=$(absolute "$key")
mkdir -p "$output"
output=$(cd "$output" && pwd -P)
command -v abuild >/dev/null && command -v readelf >/dev/null && command -v jq >/dev/null || { echo "abuild, readelf, and jq are required" >&2; exit 1; }
command -v abuild-sign >/dev/null || { echo "abuild-sign is required" >&2; exit 1; }
readelf -h "$binary" | grep -q 'Machine:.*ARM' || { echo "binary is not an ARM executable" >&2; exit 1; }
jq -e --arg id "$id" --arg version "$version" --arg executable "bin/couch-plugin-$id" \
    'type == "object" and .id == $id and .version == $version and .executable == $executable' \
    "$manifest" >/dev/null || { echo "manifest id, version, or executable does not match" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/couch-integration.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
name="couch-integration-$id"
build_output="$output/.build-$id-$$"
mkdir "$build_output"
mkdir -p "$work/$name-$version/payload/bin"
install -m 0755 "$binary" "$work/$name-$version/payload/bin/couch-plugin-$id"
install -m 0644 "$manifest" "$work/$name-$version/payload/manifest.json"
tar -C "$work/$name-$version" -czf "$work/$name-$version/$name-$version.tar.gz" payload
checksum=$(sha512sum "$work/$name-$version/$name-$version.tar.gz" | awk '{print $1}')
cat > "$work/$name-$version/APKBUILD" <<EOF
pkgname=$name
pkgver=$version
pkgrel=0
pkgdesc="Couch $id integration"
url="https://github.com/dangerouslaser/couch"
arch="armv7"
license="GPL-3.0-or-later"
options="!check !strip"
source="\$pkgname-\$pkgver.tar.gz"
builddir="\$srcdir"
package() {
    install -Dm755 "\$srcdir/payload/bin/couch-plugin-$id" "\$pkgdir/usr/lib/couch/integrations/$id/bin/couch-plugin-$id"
    install -Dm644 "\$srcdir/payload/manifest.json" "\$pkgdir/usr/lib/couch/integrations/$id/manifest.json"
}
sha512sums="$checksum  \$pkgname-\$pkgver.tar.gz"
EOF
(
    cd "$work/$name-$version"
    export PACKAGER_PRIVKEY="$key"
    # `package` alone only executes the shell function. A unique `-P` tree
    # prevents a stale same-version APK in a shared output directory from
    # being returned as this build's artifact.
    abuild -F -P "$build_output" -r >&2
    apk=$(find "$build_output" -type f -name "$name-$version-r0.apk" -print -quit)
    [ -n "$apk" ] || { echo "abuild did not produce an APK" >&2; exit 1; }
    final="$output/$name-$version-r0.apk"
    [ ! -e "$final" ] || { echo "refusing to overwrite existing $final" >&2; exit 1; }
    mv "$apk" "$final"
    printf '%s\n' "$final" > "$work/artifact"
    rm -rf "$build_output"
    : # `-P` already placed the artifact under output.
)
cat "$work/artifact"
