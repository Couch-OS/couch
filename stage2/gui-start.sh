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
# Start it last, so anything above still reports to the screen through fbcon.
# couch-gui takes the panel over when it starts and shows its own splash.
# With no saved networks the GUI opens local Wi-Fi onboarding.
GUI="$(dirname "$0")/couch-gui"
# With no network, portal.sh has already left /tmp/couch.setup behind and the
# GUI reads it at startup, showing the join QR instead of the room UI. Nothing
# to pass here: a variable set in this loop's environment could never be
# cleared again without killing the loop.
[ -f /tmp/couch.setup ] && echo "= couch-gui starting in setup mode"
# Claim the boot. init arms a 15-minute dead-man timer that reboots unless
# /tmp/stay exists - the bring-up safety net, so a build that never gets this
# far falls back to Android. The tools claim it over serial when they boot
# the device; a self-boot had nobody to do it and rebooted at 906s, twice in
# one evening. Getting here means the rootfs, WiFi and the GUI are all in
# hand, which is what "claimed" was always meant to mean.
touch /tmp/stay
# Hold a three-core hotplug floor. The keypad and touch EINT interrupts land
# only on CPU 0, and their handlers run 46-62ms in hard-IRQ with interrupts
# off (see README): whatever runs on CPU 0 during a key press stalls for that
# long, and at the default single online core that is the UI, every press. The
# scheduler migrates the runnable UI off CPU 0 when it is saturated by that IRQ
# time, but only if another core is online - and with a two-core floor the one
# spare is too often the one the scheduler also parks work on, so a 46ms hitch
# still leaks through. Three keeps the UI reliably clear of the frozen core.
# The cost is two cores' idle power; drop this to 2, or remove it, to trade
# smoothness back for battery. Load-based hotplug still brings the fourth.
# init keeps a loop that rewrites 255 to both backlights every five seconds -
# a bring-up habit from when the panel seemed to switch itself off (it does
# not; measured, an unattended level holds). The GUI owns brightness now, so
# the loop is stopped: it is the child of init whose own child is a `sleep 5`.
# One pass over /proc rather than one per pair: this core is slow.
for p in /proc/[0-9]*; do
    echo "$(basename $p) $(awk '{print $4}' $p/stat 2>/dev/null) $(tr '\0' ' ' < $p/cmdline 2>/dev/null | cut -c1-40)"
done > /tmp/ptab
KEEPER=$(awk 'NR==FNR{pp[$1]=$2; next} /sleep 5/{if (pp[pp[$1]]==1) print pp[$1]}' /tmp/ptab /tmp/ptab | head -1)
[ -n "$KEEPER" ] && kill "$KEEPER" 2>/dev/null && echo "= stopped init's backlight keeper (pid $KEEPER)"
[ -w /proc/hps/num_base_perf_serv ] && echo 3 > /proc/hps/num_base_perf_serv
for c in 1 2; do [ -w /sys/devices/system/cpu/cpu$c/online ] && echo 1 > /sys/devices/system/cpu/cpu$c/online; done
if [ -x "$GUI" ]; then
    ( while true; do
        "$GUI" >/tmp/gui.log 2>&1
        echo "= couch-gui exited ($?), restarting" >> /tmp/gui.log
        $BB sleep 2
      done ) &
    echo "= couch-gui started"
    # The GUI owns the panel from here, so the rest of the boot narration goes
    # to the log instead of on top of it. fbcon cannot be relied on to stop by
    # itself: stage1 resolves $FBCON before /mnt/alpine is mounted, so it always
    # runs the copy baked into the initramfs, not the one we can update here.
    exec >>/tmp/stage2.log 2>&1
else
    echo "= no couch-gui at $GUI"
fi

$BB dmesg | $BB dd of=$LOG bs=512 seek=$DMESG2_SECTOR conv=notrunc 2>/dev/null
mark $((BASE+4)) "S4 stage2 done"
echo ""
echo "= READY  uptime $($BB cut -d. -f1 /proc/uptime)s"
echo "= edit:  tools/push.py stage2/stage2.sh /mnt/alpine/opt/couch/stage2.sh"
echo "= rerun: tools/relinux.sh"
