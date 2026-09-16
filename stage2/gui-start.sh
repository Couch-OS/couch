#!/bin/busybox sh
# Sourced after system-service startup; skipped entirely by recovery.
# The configuration editor uses Alpine paths and the shared /tmp pairing PIN.
# Keep it supervised independently of the physical display process.
if [ -x "$BASE_DIR/couch-confd" ] && [ -f "$BASE_DIR/confd.sh" ]; then
    $BB chroot "$A" /bin/sh "${BASE_DIR#$A}/confd.sh" </dev/null >/tmp/confd-supervisor.log 2>&1 &
    echo "= configuration editor on port 8090"
fi

# The static GUI runs in the initramfs, while DHCP maintains Alpine's resolver.
# Share paths rather than copying: network changes can replace these files.
# Without this bridge IP-based TV controls work but hostname artwork fails.
$BB mkdir -p /etc
$BB ln -sf "$A/etc/resolv.conf" /etc/resolv.conf
$BB ln -sf "$A/etc/hosts" /etc/hosts

# --- the UI ------------------------------------------------------------------
# Started as soon as the panel, the input nodes and the Alpine root are ready,
# which is well before the radio. Everything above still reports to the screen
# through fbcon; the radio block stage2.sh backgrounded already writes to
# /tmp/stage2.log instead, because couch-gui takes the panel over here and shows
# its own splash. With no saved networks the GUI still opens local Wi-Fi
# onboarding, just when the setup decision lands rather than at its first frame.
GUI="$(dirname "$0")/couch-gui"
# Setup mode is /tmp/couch.setup, which portal.sh writes and the GUI re-reads
# every second, so a hotspot the background setup decision asks for after this
# point is picked up too. Nothing to pass here: a variable set in this loop's
# environment could never be cleared again without killing the loop.
[ -f /tmp/couch.setup ] && echo "= couch-gui starting in setup mode"
# Keep the established three-core floor while the display is active. The GUI
# releases it to one after panel power-down and restores it before wake; HPS
# can still add cores for background work. Reset it on every GUI start so a
# crash in standby cannot leave the replacement GUI with the standby floor.
# init keeps a loop that rewrites 255 to both backlights every five seconds -
# a bring-up habit from when the panel seemed to switch itself off (it does
# not; measured, an unattended level holds). The GUI owns brightness now, so
# the loop is stopped. init records its pid in /tmp/backlight-keeper.pid on
# images from #135 on; older images are found by shape: the child of init
# whose own child is a `sleep 5`. That shape only exists while the loop is
# inside its sleep, so a single scan could land in the gap between sleeps and
# miss it - and a missed keeper relights the key LEDs every five seconds with
# the screen off (the LCD write does nothing to a powered-down panel, so only
# the keys showed it). Scan until found, then confirm it is gone.
keeper_scan() {
    for p in /proc/[0-9]*; do
        echo "$(basename $p) $(awk '{print $4}' $p/stat 2>/dev/null) $(tr '\0' ' ' < $p/cmdline 2>/dev/null | cut -c1-40)"
    done > /tmp/ptab
    awk 'NR==FNR{pp[$1]=$2; next} /sleep 5/{if (pp[pp[$1]]==1) print pp[$1]}' /tmp/ptab /tmp/ptab | head -1
}
KEEPER=$($BB cat /tmp/backlight-keeper.pid 2>/dev/null)
[ -n "$KEEPER" ] && [ -d "/proc/$KEEPER" ] || KEEPER=
n=0
while [ -z "$KEEPER" ] && [ "$n" -lt 12 ]; do
    KEEPER=$(keeper_scan)
    [ -n "$KEEPER" ] || $BB sleep 0.5
    n=$((n+1))
done
if [ -n "$KEEPER" ]; then
    kill "$KEEPER" 2>/dev/null
    $BB sleep 0.2
    if [ -d "/proc/$KEEPER" ]; then
        kill -9 "$KEEPER" 2>/dev/null
        echo "= init's backlight keeper (pid $KEEPER) needed SIGKILL"
    else
        echo "= stopped init's backlight keeper (pid $KEEPER)"
    fi
else
    echo "= init's backlight keeper not found after $n scans; the key LEDs may relight every 5 s"
fi
if [ -x "$GUI" ]; then
    ( while true; do
        [ -w /proc/hps/num_base_perf_serv ] && echo 3 > /proc/hps/num_base_perf_serv
        for c in 1 2; do [ -w /sys/devices/system/cpu/cpu$c/online ] && echo 1 > /sys/devices/system/cpu/cpu$c/online; done
        "$GUI" >/tmp/gui.log 2>&1
        echo "= couch-gui exited ($?), restarting" >> /tmp/gui.log
        $BB sleep 2
      done ) &
    echo "= couch-gui started"
    # Claim the boot. init arms a 15-minute dead-man timer that reboots unless
    # /tmp/stay exists - the bring-up safety net, so a build that never gets
    # this far falls back to Android. The tools claim it over serial when they
    # boot the device; a self-boot had nobody to do it and rebooted at 906s,
    # twice in one evening. Inside this branch because "claimed" means the
    # rootfs and the GUI are in hand: touched ahead of the -x test, a runtime
    # with no couch-gui disarmed the dead-man and was then left to init's 150s
    # health gate instead of falling back to Android. WiFi is no longer part of
    # the claim - it comes up beside the GUI now - and never belonged in it: a
    # remote that renders and answers its keypad is not an Android fallback
    # case because it missed a lease.
    touch /tmp/stay
    # The GUI owns the panel from here, so the rest of the boot narration goes
    # to the log instead of on top of it. fbcon cannot be relied on to stop by
    # itself: stage1 resolves $FBCON before /mnt/alpine is mounted, so it always
    # runs the copy baked into the initramfs, not the one we can update here.
    exec >>/tmp/stage2.log 2>&1
else
    echo "= no couch-gui at $GUI"
fi

# S4 says this script is done, not that the boot is: the radio block and its
# dmesg snapshot are still running behind it, and mark() stamps the uptime, so
# S4 now reads earlier than S5/S6 above it.
mark $((BASE+4)) "S4 stage2 done"
echo ""
echo "= READY  uptime $($BB cut -d. -f1 /proc/uptime)s"
echo "= edit:  tools/push.py stage2/stage2.sh /mnt/alpine/opt/couch/stage2.sh"
echo "= rerun: tools/relinux.sh"
