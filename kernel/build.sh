#!/bin/sh
# Build locally or forward an explicitly configured remote builder.
set -eu
cd "$(dirname "$0")/.."
LOCAL_CONFIG=${COUCH_LOCAL_CONFIG:-local.env}
if [ -f "$LOCAL_CONFIG" ]; then
    # Deliberately local-only operator configuration; see local.env.example.
    # Export assignments so the explicitly selected remote helper receives them.
    set -a
    . "$LOCAL_CONFIG"
    set +a
fi
MODE=${KERNEL_BUILD_MODE:-local}
case "${1:-}" in
    --local) MODE=local; shift ;;
    --remote) MODE=remote; shift ;;
esac
PROFILE=${1:-normal}
case "$PROFILE" in normal|diagnostic) ;; *) echo "usage: $0 [--local|--remote] [normal|diagnostic]" >&2; exit 2;; esac
case "$MODE" in
    local) ;;
    remote) exec python3 kernel/remote-build.py "$PROFILE" ;;
    *) echo "KERNEL_BUILD_MODE must be local or remote" >&2; exit 2 ;;
esac
: "${KTREE:?set KTREE to a clean couch-kernel checkout (or configure local.env)}"
KOUT=${KOUT:-$(dirname "$KTREE")/out-$PROFILE}
KIMAGE=${KIMAGE:-couch-kbuild}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)}
KBUILD_BUILD_HOST=${KBUILD_BUILD_HOST:-couch-builder}
export KBUILD_BUILD_HOST
[ -f "$KTREE/Makefile" ] || { echo "no kernel tree at $KTREE" >&2; exit 1; }
mkdir -p "$KOUT"
KTREE=$(cd "$KTREE" && pwd)
KOUT=$(cd "$KOUT" && pwd)
exec 9>"$KOUT/.build.lock"
flock -n 9 || { echo "build already running in $KOUT" >&2; exit 1; }
SOURCE_COMMIT=$(git -C "$KTREE" rev-parse HEAD)
[ -z "$(git -C "$KTREE" status --porcelain)" ] || {
    echo "commit kernel source changes before building" >&2; exit 1;
}
[ "$PROFILE" != normal ] || python3 kernel/source_policy.py "$KTREE"
KIMAGE=$(docker image inspect "$KIMAGE" --format '{{.Id}}')
KBUILD_BUILD_TIMESTAMP=$(git -C "$KTREE" show -s --format=%cD HEAD)
export KBUILD_BUILD_TIMESTAMP
docker run --rm --user "$(id -u):$(id -g)" \
    -e KBUILD_BUILD_TIMESTAMP -e KBUILD_BUILD_USER=couch -e KBUILD_BUILD_HOST \
    -e KBUILD_BUILD_VERSION=1 -e PROFILE="$PROFILE" -e JOBS="$JOBS" \
    -e LOCALVERSION="-g$(git -C "$KTREE" rev-parse --short=12 HEAD)" \
    -v "$PWD/kernel":/recipe:ro -v "$KTREE":/src -v "$KOUT":/out -w /src "$KIMAGE" sh -ec '
    python3 /recipe/configure.py /recipe/couch-ha100.config /recipe/profiles/$PROFILE.config /out/.config
    make O=/out ARCH=arm CROSS_COMPILE=arm-eabi- olddefconfig > /out/config.log 2>&1 || {
        tail -30 /out/config.log; exit 1;
    }
    python3 /recipe/configure.py --check /recipe/profiles/$PROFILE.config /out/.config
    make O=/out ARCH=arm CROSS_COMPILE=arm-eabi- -j"$JOBS" zImage
    arm-eabi-gcc --version > /out/compiler.txt
    sha256sum /opt/arm-eabi-4.9/bin/arm-eabi-gcc > /out/compiler.sha256
    '
[ "$(git -C "$KTREE" rev-parse HEAD)" = "$SOURCE_COMMIT" ] && \
    [ -z "$(git -C "$KTREE" status --porcelain)" ] || {
    echo "kernel source changed during the build; discard these outputs" >&2; exit 1;
}
python3 kernel/manifest.py "$KTREE" "$KOUT" "$PROFILE" "$KIMAGE"
ls -lh "$KOUT/arch/arm/boot/zImage"
