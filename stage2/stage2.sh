#!/bin/busybox sh
# Rootfs entry point. A failed runtime leaves the initramfs recovery shell intact.
BASE_DIR=$(dirname "$0")
if [ "${COUCH_RUNTIME_SELECTED:-0}" != 1 ]; then
    exec /bin/busybox sh "$BASE_DIR/runtime-boot.sh"
fi
# The kernel's name, for the DHCP lease and logs; couch.local itself is
# answered by the config daemon over mDNS, which needs no hostname.
/bin/busybox hostname couch 2>/dev/null
# Loopback. Nothing before this ever raised it, and Linux does not do so by
# itself, so 127.0.0.1 did not exist on the remote: the mDNS responder's
# wake-up socket (and any other loopback bind) failed with "Address not
# available", which is why couch.local and TV discovery never answered.
/bin/busybox ifconfig lo 127.0.0.1 netmask 255.0.0.0 up 2>/dev/null
. "$BASE_DIR/hardware-init.sh"
# hardware-init.sh defines radio_up() instead of running it. Recovery runs this
# script for its connectivity alone and has no UI to hold back, so it keeps the
# old serial order; a normal boot starts the radio further down, beside the GUI.
[ "${COUCH_NO_UI:-0}" = "1" ] && radio_up

# The system service is independent of the GUI and remains available in recovery.
$BB sh "$(dirname "$0")/system.sh" </dev/null >/tmp/system-supervisor.log 2>&1 &
n=0
while [ ! -S /tmp/couch-system/control.sock ] && [ $n -lt 50 ]; do
    $BB sleep 0.1; n=$((n+1))
done
SYSTEM="$(dirname "$0")/couch-system"

# Everything that needs the radio's answer, and nothing else. It runs after
# radio_up, which on a normal boot means inside the background job, so none of
# this is in front of the GUI any more.
network_settle() {
    # Count saved credentials independently of radio startup. A missing interface
    # is a hardware startup failure, not a request to enter Wi-Fi credentials again.
    SAVED_NETS=$($BB grep -c '^network=' /mnt/alpine/opt/couch/networks.conf 2>/dev/null)
    [ "${SAVED_NETS:-0}" -gt "${NETS:-0}" ] && NETS=$SAVED_NETS
    SETUP_MODE=$($BB sh "$(dirname "$0")/setup-mode.sh" "${NETS:-0}" "${COUCH_NO_UI:-0}" "${COUCH_SETUP_AP:-0}")
    if [ "$SETUP_MODE" = recovery ]; then
        "$SYSTEM" hotspot >/tmp/portal.log 2>&1
    elif [ "$SETUP_MODE" = local ]; then
        : > /tmp/couch.onboarding
    fi
    [ -n "$IP" ] && "$SYSTEM" ssh-start >>/tmp/system.log 2>&1
    # Last, and after the onboarding marker above: the GUI looks for
    # couch.onboarding first, so the pair can never read as "decided, no
    # onboarding" while the decision is still being written.
    $BB rm -f /tmp/couch.network-pending
}
# Bluetooth is NOT started here. Wi-Fi and Bluetooth share one combo radio and
# transport; powering Bluetooth on while Wi-Fi is still coming up contends for
# it and can reset the whole chip, dropping Wi-Fi. The Settings/web toggle
# starts it live, by which time Wi-Fi is already associated. See docs/bluetooth.md.

# Recovery runs this script for its connectivity alone. It has no UI to start,
# and stopping here leaves the USB serial shell and sshd in charge - which is
# the whole point of that image: a way back in when the slot under test does not
# boot. Everything above (vendor blobs, wmt modules, wifi, dhcp, sshd) is shared
# with a normal boot rather than duplicated into a second script that would rot.
if [ "${COUCH_NO_UI:-0}" = "1" ]; then
    network_settle
    mark $((BASE+4)) "S4 stage2 done (recovery, no ui)"
    echo "= recovery: ${IP:+network up on $IP}${IP:+, }${IP:-no network, }UI skipped"
    exit 0
fi

# The GUI needs the panel, the input nodes and the Alpine root; the radio owns
# none of those, so it comes up beside the GUI rather than in front of it. Its
# narration goes to the log because couch-gui takes the panel a moment from here
# and fbcon would draw on top of it. The marker says "the setup decision has not
# been made yet", so a remote with saved networks does not flash Wi-Fi
# onboarding while it is still associating; network_settle removes it.
: > /tmp/couch.network-pending
( radio_up
  network_settle
  # The boot's second dmesg snapshot. It used to sit at the end of gui-start.sh,
  # which now finishes before the radio does - taken there it would cut the
  # whole WiFi bring-up out of the one log that survives a reset.
  $BB dmesg | $BB dd of=$LOG bs=512 seek=$DMESG2_SECTOR conv=notrunc 2>/dev/null
) </dev/null >>/tmp/stage2.log 2>&1 &

. "$BASE_DIR/gui-start.sh"
