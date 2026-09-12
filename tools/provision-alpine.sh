#!/bin/sh
# Build the base Alpine rootfs on the cache partition, from a clean device.
#
# WiFi packages are baked in deliberately: without wpa_supplicant already present
# there is no network, and without a network apk cannot fetch wpa_supplicant. The
# packages are side-loaded offline over adb, which also makes the base image
# reproducible rather than something assembled by hand.
#
# Run from Android. Destroys whatever is on the cache partition.
set -e
cd "$(dirname "$0")/.."
. tools/env.sh

SYSTEM_BIN=daemon/target/armv7-unknown-linux-musleabihf/release/couch-system
[ -f build/couch-wmt-properties.so ] || { echo "Run tools/build-wmt-properties.sh before provisioning."; exit 1; }
[ -x "$SYSTEM_BIN" ] || { echo "Build couch-system for ARMv7 before provisioning."; exit 1; }

ALPINE_VER=3.21
ALPINE_REL=3.21.7
MIRROR=https://dl-cdn.alpinelinux.org/alpine
ROOTFS=alpine-minirootfs-$ALPINE_REL-armv7.tar.gz
BB=/data/local/tmp/busybox
MNT=/data/local/tmp/alp

# Alpine's own wpa_supplicant, not the vendor one: the vendor build logs solely
# to Android's logd and exits silently when logd is not running, which makes it
# undebuggable here.
# Everything the device needs to reach a network on its own, plus the portal
# that configures it when it cannot.
PKGS="wpa_supplicant openssh hostapd dnsmasq iw tzdata"
# Hand-fetched fallback set: just enough to bring WiFi up if there is no network
# during provisioning. apk fetch --recursive builds the real closure below.
SEED="wpa_supplicant libnl3 dbus-libs pcsc-lite-libs"

mkdir -p build/apks

[ -f "build/$ROOTFS" ] || {
    echo "fetching $ROOTFS ..."
    curl -sL --max-time 300 -o "build/$ROOTFS" "$MIRROR/v$ALPINE_VER/releases/armv7/$ROOTFS"
}

echo "resolving packages ..."
IDX=$(curl -s --max-time 60 "$MIRROR/v$ALPINE_VER/main/armv7/")
for pkg in $SEED; do
    f=$(echo "$IDX" | grep -oE "${pkg}-[0-9][^\"]*\.apk" | head -1)
    [ -n "$f" ] || { echo "cannot resolve $pkg"; exit 1; }
    [ -f "build/apks/$f" ] || curl -sL --max-time 120 -o "build/apks/$f" \
        "$MIRROR/v$ALPINE_VER/main/armv7/$f"
    printf '  %s\n' "$f"
done

sh "$(dirname "$0")/wait-adb.sh" 60 || exit 1
$ADB shell "[ -x $BB ]" 2>/dev/null || { $ADB push build/busybox-armv7l $BB >/dev/null; $ADB shell "chmod 755 $BB"; }

echo "formatting cache and unpacking rootfs ..."
$ADB shell "umount /cache 2>/dev/null; umount $MNT 2>/dev/null; \
    mke2fs -t ext4 -b 4096 -L alpine -F $PART_CACHE >/dev/null 2>&1; \
    mkdir -p $MNT && mount -t ext4 $PART_CACHE $MNT"
$ADB push "build/$ROOTFS" /data/local/tmp/rootfs.tgz >/dev/null
# busybox tar, not Android's toybox: symlinks and permissions must survive.
$ADB shell "cd $MNT && $BB tar xzf /data/local/tmp/rootfs.tgz"

echo "installing packages offline ..."
$ADB shell "mkdir -p $MNT/opt/apks $MNT/opt/couch"
for f in build/apks/*.apk; do $ADB push "$f" "$MNT/opt/apks/" >/dev/null; done
$ADB shell "cd $MNT && printf '%s\n' \
    '$MIRROR/v$ALPINE_VER/main' '$MIRROR/v$ALPINE_VER/community' > etc/apk/repositories
    echo 'nameserver 1.1.1.1' > etc/resolv.conf
    echo couch > etc/hostname"
# apk runs inside the chroot so it resolves against the new root, not Android.
# Prefer a networked install: the offline apks are only the named packages, not
# their dependency closure, so an offline-only install of openssh or hostapd
# fails on transitive dependencies. Android's WiFi gives the chroot a network,
# and the offline set remains the fallback for a device with no connectivity.
$ADB shell "$BB chroot $MNT /bin/sh -c '
    export PATH=/sbin:/usr/sbin:/bin:/usr/bin
    if apk update >/dev/null 2>&1; then
        echo \"apk: installing over the network\"
        apk add $PKGS 2>&1 | tail -3
        # Cache the full dependency closure inside the image so a later
        # provision, or a device with no connectivity, can install offline.
        apk fetch --recursive -o /opt/apks $PKGS >/dev/null 2>&1
        echo \"cached \$(ls /opt/apks | wc -l) packages for offline provisioning\"
    else
        echo \"apk: no network, using the seed set\"
        apk add --allow-untrusted --no-network /opt/apks/*.apk 2>&1 | tail -3
    fi'"

echo "installing our payload ..."
$ADB push build/couch-wmt-properties.so "$MNT/opt/couch/couch-wmt-properties.so" >/dev/null
$ADB push "$SYSTEM_BIN" "$MNT/opt/couch/couch-system" >/dev/null
for f in stage2/*.sh; do
    $ADB push "$f" "$MNT/opt/couch/$(basename "$f")" >/dev/null
done
$ADB shell "mkdir -p $MNT/opt/couch/www/cgi-bin"
$ADB push stage2/www/index.html "$MNT/opt/couch/www/index.html" >/dev/null
for f in stage2/www/cgi-bin/*; do
    $ADB push "$f" "$MNT/opt/couch/www/cgi-bin/$(basename "$f")" >/dev/null
done
[ -f build/fbcon ] && $ADB push build/fbcon "$MNT/opt/couch/fbcon" >/dev/null

# The MediaTek blobs travel with the image, so the installed system does not
# need Android's partitions to bring up WiFi.
if [ -d build/vendor-bundle/vendor/lib/modules ]; then
    echo "installing vendor blobs ..."
    $ADB push build/vendor-bundle/vendor "$MNT/opt/couch/" >/dev/null
    $ADB push build/vendor-bundle/system "$MNT/opt/couch/" >/dev/null
else
    echo "WARNING: no vendor bundle - run tools/extract-vendor.sh, or WiFi will"
    echo "         depend on Android still being installed"
fi

# sshd: key-only, no passwords, and NO key shipped. The image trusts nobody
# until someone enrols a key through the setup portal (which requires reading a
# passphrase off the device's screen and pressing a button on it).
#
# Alpine ships root locked with "!" in /etc/shadow, and sshd refuses to complete
# authentication for a locked account even with a valid key. "*" leaves it with
# no password while unlocked, so key auth works and password auth cannot.
$ADB shell "mkdir -p $MNT/etc/ssh/sshd_config.d
    printf '%s\n' 'PermitRootLogin prohibit-password' 'PasswordAuthentication no' \
        'PubkeyAuthentication yes' 'UseDNS no' > $MNT/etc/ssh/sshd_config.d/couch.conf
    sed -i 's|^root:[!*]*:|root:*:|' $MNT/etc/shadow
    rm -rf $MNT/root/.ssh
    rm -f $MNT/opt/couch/networks.conf"

$ADB shell "chmod -R 755 $MNT/opt/couch; chmod 644 $MNT/opt/couch/www/index.html; \
    $BB chroot $MNT /sbin/apk info 2>/dev/null | tr '\n' ' '; echo; \
    echo \"authorized_keys present: \$([ -e $MNT/root/.ssh/authorized_keys ] && echo YES-BAD || echo no)\"; \
    sync; umount $MNT && echo 'provisioned, unmounted'"

echo
echo "The image trusts no keys. Enrol one via the setup portal, or for a"
echo "development device:  tools/push.py ~/.ssh/id_ed25519.pub /root/.ssh/authorized_keys"
