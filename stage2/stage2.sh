#!/bin/busybox sh
# Rootfs entry point. A failed runtime leaves the initramfs recovery shell intact.
BASE_DIR=$(dirname "$0")
if [ "${COUCH_RUNTIME_SELECTED:-0}" != 1 ]; then
    exec /bin/busybox sh "$BASE_DIR/runtime-boot.sh"
fi
. "$BASE_DIR/hardware-init.sh"

# The system service is independent of the GUI and remains available in recovery.
$BB sh "$(dirname "$0")/system.sh" </dev/null >/tmp/system-supervisor.log 2>&1 &
n=0
while [ ! -S /tmp/couch-system/control.sock ] && [ $n -lt 50 ]; do
    $BB sleep 0.1; n=$((n+1))
done
SYSTEM="$(dirname "$0")/couch-system"
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

# Recovery runs this script for its connectivity alone. It has no UI to start,
# and stopping here leaves the USB serial shell and sshd in charge - which is
# the whole point of that image: a way back in when the slot under test does not
# boot. Everything above (vendor blobs, wmt modules, wifi, dhcp, sshd) is shared
# with a normal boot rather than duplicated into a second script that would rot.
if [ "${COUCH_NO_UI:-0}" = "1" ]; then
    mark $((BASE+4)) "S4 stage2 done (recovery, no ui)"
    echo "= recovery: ${IP:+network up on $IP}${IP:+, }${IP:-no network, }UI skipped"
    exit 0
fi


. "$BASE_DIR/gui-start.sh"
