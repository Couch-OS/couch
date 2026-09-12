#!/bin/busybox sh
# Build a wpa_supplicant config from Android's own saved networks.
#
# Emits every stored network, not just the first: the first entry in the store is
# not necessarily the one in range, and wpa_supplicant will pick whichever it can
# actually see. Credentials are read on the device at runtime and never leave it.
CFG=${1:-/mnt/data/misc/wifi/WifiConfigStore.xml}
OUT=${2:-/tmp/wpa.conf}
# Android's store is a bonus, not a requirement: a distributable image may be
# running on a device that was never set up under Android.
[ -f "$CFG" ] || CFG=/dev/null

# Next to this script, deliberately. An absolute /opt/couch path is only right
# inside the Alpine chroot, which is where the portal's CGI writes it; stage2
# runs this from the initramfs, where the very same file is at
# /mnt/alpine/opt/couch - so the lookup quietly found nothing and every network
# saved through the portal was dropped on the next boot.
SAVED=/opt/couch/networks.conf
[ -d /mnt/alpine/opt/couch ] && SAVED=/mnt/alpine/opt/couch/networks.conf

{
    echo "ctrl_interface=/tmp/wpa"
    echo "update_config=1"
    # Networks configured through the setup portal, which persist on the Alpine
    # partition, come first.
    [ -f "$SAVED" ] && grep -v '^# net ' "$SAVED"
    awk '
    function flush() {
        if (have) {
            printf "network={\n  ssid=\"%s\"\n", ssid
            if (psk != "") printf "  psk=\"%s\"\n", psk
            else print "  key_mgmt=NONE"
            print "}"
        }
    }
    /<string name="SSID">/ {
        flush()
        s = $0
        sub(/.*<string name="SSID">&quot;/, "", s)
        sub(/&quot;<\/string>.*/, "", s)
        ssid = s; psk = ""; have = 1
    }
    /<string name="PreSharedKey">/ {
        s = $0
        sub(/.*<string name="PreSharedKey">&quot;/, "", s)
        sub(/&quot;<\/string>.*/, "", s)
        psk = s
    }
    END { flush() }
    ' "$CFG"
} > "$OUT"
chmod 600 "$OUT"
echo "networks: $(grep -c '^network=' "$OUT")"
grep '  ssid=' "$OUT"
