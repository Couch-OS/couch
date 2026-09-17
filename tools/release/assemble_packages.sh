#!/bin/sh
# Isolated container only: /tmp is disposable, input/cache mounts read-only.
set -eu
cd /packages
sha256sum -c /input/package-hashes.txt > /out/package-checks.txt
apk --repositories-file /dev/null --keys-dir /usr/share/apk/keys/armv7 verify packages/*.apk >> /out/package-checks.txt
mkdir /tmp/root
cd /tmp/root
tar -xzf /input/rootfs-staging.tar.gz
# The host already registered ARM binfmt. No host binfmt or device mounts here.
# Run an actual ARM executable before touching the package database.
chroot /tmp/root /bin/busybox true
apk --root /tmp/root --arch armv7 --keys-dir /usr/share/apk/keys/armv7 \
    --repositories-file /dev/null --no-network --force-non-repository \
    add /packages/packages/*.apk > /out/package-install.txt 2>&1
# Packages may create default configuration; preserve clean Couch defaults.
# SSH host keys must only be generated after enrollment, never by the builder.
for key in /tmp/root/etc/ssh/ssh_host_*; do
    [ ! -e "$key" ] || { echo 'Package installation generated SSH host keys' >&2; exit 1; }
done
# dbus's signed post-install hook creates a builder-local identity. Remove only
# this expected generated regular file; the public rootfs validator continues
# to reject either machine-id path and all other private state. couch-system
# creates a persistent per-device identity when D-Bus is first needed.
machine_id=/tmp/root/etc/machine-id
if [ -e "$machine_id" ] || [ -L "$machine_id" ]; then
    [ -f "$machine_id" ] && [ ! -L "$machine_id" ] &&
        [ "$(wc -c < "$machine_id")" -eq 33 ] &&
        [ "$(tail -c 1 "$machine_id" | wc -l)" -eq 1 ] &&
        grep -Eq '^[0-9a-f]{32}$' "$machine_id" || {
        echo 'Unexpected package-generated machine ID' >&2; exit 1;
    }
    rm "$machine_id"
fi
chroot /tmp/root /sbin/wpa_supplicant -v > /out/runtime-checks.txt
chroot /tmp/root /usr/sbin/sshd -V >> /out/runtime-checks.txt 2>&1
# CoreELEC OS controls require OpenSSH client options, not Dropbear. -G only
# expands configuration; this fixture address is never contacted.
chroot /tmp/root /usr/bin/ssh -V >> /out/runtime-checks.txt 2>&1
chroot /tmp/root /usr/bin/ssh -F none -G -o BatchMode=yes \
    -o StrictHostKeyChecking=yes -o IdentityAgent=none \
    -o ClearAllForwardings=yes root@192.0.2.1 > /out/ssh-client-check.txt
chroot /tmp/root /usr/sbin/iw --version >> /out/runtime-checks.txt
# Some maintainer scripts redirect to /dev/null before devtmpfs exists. It may
# be a temporary regular file here; no /dev runtime contents belong in the image.
rm -f /tmp/root/dev/null
cd /tmp/root
tar -czf /out/package-rootfs.raw.tar.gz .
