#!/bin/sh
# Shared by normal and recovery image builders so neither reuses stale artwork.
set -eu
cd "$(dirname "$0")/.."
output=${1:-build/fbcon}
if [ -f "$output" ] && [ ! src/fbcon.c -nt "$output" ] && \
   [ ! src/logo.h -nt "$output" ] && [ ! src/font.h -nt "$output" ]; then
    exit 0
fi
case "$(uname -s)" in
    Darwin) host=darwin-x86_64; default_ndk="$HOME/Library/Android/sdk/ndk/29.0.14206865";;
    Linux) host=linux-x86_64; default_ndk="$HOME/Android/Sdk/ndk/29.0.14206865";;
    *) echo 'Build fbcon on Linux or macOS with an Android NDK.' >&2; exit 1;;
esac
NDK=${NDK:-${ANDROID_NDK_HOME:-$default_ndk}}
bin="$NDK/toolchains/llvm/prebuilt/$host/bin"
compiler="$bin/armv7a-linux-androideabi21-clang"
[ -x "$compiler" ] && [ -x "$bin/llvm-strip" ] || {
    echo 'fbcon needs rebuilding; set NDK to an ARM Android toolchain.' >&2
    exit 1
}
mkdir -p "$(dirname "$output")"
pending="$output.pending.$$"
trap 'rm -f "$pending"' EXIT HUP INT TERM
echo 'building fbcon...'
"$compiler" -static -Os -o "$pending" src/fbcon.c
"$bin/llvm-strip" "$pending"
mv -f "$pending" "$output"
