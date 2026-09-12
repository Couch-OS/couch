#!/bin/busybox sh
# Sourced by stage2.sh: HA100 vendor initialization and boot-time station setup.
# Keep hardware ordering here; runtime changes belong to couch-system.
#
# The initramfs has already mounted /proc, /sys, /tmp and Alpine, opened the USB
# serial shell and started fbcon, so a broken edit here still leaves a shell.

BB=/bin/busybox
LOG=/dev/mmcblk0p13      # expdb: scratch for markers and logs
# Above MTK's crash dump, and offset from init's block so the two stages do
# not overwrite each other's markers (init owns BASE 13000-13005). Keep in
# step with MARKER_SECTOR / DMESG2_SECTOR in tools/env.sh.
BASE=13006
DMESG2_SECTOR=15200
mark() { $BB echo "[$($BB cut -d. -f1 /proc/uptime)s] $2" | $BB dd of=$LOG bs=512 seek=$1 count=1 conv=notrunc 2>/dev/null; }

echo "= stage2 running from $(dirname "$0")"
[ -e /dev/mmcblk0p13 ] || $BB mknod /dev/mmcblk0p13 b 179 13 2>/dev/null

# The custom IR driver is a misc device; stock firmware used major 243.
# Resolve the registered device number instead of retaining the cpio's stock
# node (which may now refer to an unrelated character driver).
if [ -r /sys/class/misc/irtx/dev ]; then
    ir_dev=$($BB cat /sys/class/misc/irtx/dev)
    case "$ir_dev" in
        *[!0-9:]*|:*|*:) echo "= invalid IR device number: $ir_dev" ;;
        *:*) $BB rm -f /dev/irtx
             $BB mknod -m 600 /dev/irtx c "${ir_dev%:*}" "${ir_dev#*:}" ;;
    esac
else
    # No IR driver: do not leave a stock node pointing at another driver.
    $BB rm -f /dev/irtx
fi

# MediaTek's connectivity blobs. Couch ships its own copies so it does not
# depend on Android still being installed: mounting Android's /system and
# /vendor works only until someone flashes over them, and then WiFi disappears
# and takes the setup portal with it. Android's partitions are used only as a
# fallback for a device that still has them.
$BB mkdir -p /system /vendor /dev/__properties__ /usr/share/udhcpc
# Vendor firmware is an immutable OS input, outside signed application slots.
BUNDLE=/mnt/alpine/opt/couch
if [ -d "$BUNDLE/vendor/lib/modules" ]; then
    $BB mount -o bind "$BUNDLE/vendor" /vendor 2>/dev/null
    $BB mount -o bind "$BUNDLE/system" /system 2>/dev/null
    VSRC=bundled
else
    $BB mount -t ext4 -o ro /dev/mmcblk0p21 /system 2>/dev/null
    $BB mount -t ext4 -o ro /dev/mmcblk0p14 /vendor 2>/dev/null
    VSRC=android-partitions
fi
echo "= vendor blobs: $VSRC"

# With no devtmpfs only the nodes baked into the cpio exist. wmt_launcher opens
# /dev/ttyMT2 for the chip's STP transport, and without it the chip never
# configures and power-on fails with -3.
for spec in ttyMT1:210 ttyMT2:211 ttyMT3:212; do
    n=${spec%%:*}; m=${spec##*:}
    [ -e /dev/$n ] || $BB mknod /dev/$n c 204 $m 2>/dev/null
done
# Android satisfies these with symlinks; the bundle has to as well.
[ -e /system/vendor ]      || $BB ln -s /vendor /system/vendor 2>/dev/null
[ -e /system/etc/firmware ] || { $BB mkdir -p /system/etc 2>/dev/null
                                 $BB ln -s /vendor/firmware /system/etc/firmware 2>/dev/null; }
# The gen2 wlan driver opens its RAM code by a hardcoded path,
# /etc/firmware/WIFI_RAM_CODE_6580, which Android satisfies through its
# /etc -> /system/etc link. Alpine has no /etc/firmware at all, and the kernel
# thread resolves the path against this (initramfs) root. Without the link
# wlanProbe fails inside kalFirmwareImageMapping and wmt reports only
# "wlan probe fail(-1)".
[ -e /etc/firmware ] || { $BB mkdir -p /etc 2>/dev/null
                          $BB ln -s /vendor/firmware /etc/firmware 2>/dev/null; }
echo "= nodes: ttyMT2 $([ -e /dev/ttyMT2 ] && echo ok || echo MISSING), /system/vendor $([ -e /system/vendor ] && echo ok || echo MISSING)"
mark $((BASE+1)) "S1 vendor=$VSRC bundle=$BUNDLE mods=$($BB ls /vendor/lib/modules 2>/dev/null | $BB wc -l)"
$BB mount -t tmpfs tmpfs /dev/__properties__ 2>/dev/null
( cd /dev/__properties__ && $BB tar xzf "$PROPS" 2>/dev/null )
echo "= /system $([ -x /system/bin/linker ] && echo ok || echo FAIL)  /vendor $([ -d /vendor/lib/modules ] && echo ok || echo FAIL)  props $($BB ls /dev/__properties__ | $BB wc -l)"

# Expose the Android tree inside the chroot for interactive debugging.
A=/mnt/alpine
$BB mkdir -p $A/system $A/vendor
$BB mount -o bind /system $A/system 2>/dev/null
$BB mount -o bind /vendor $A/vendor 2>/dev/null
# A bind of /dev does not carry its submounts, so the property area is invisible
# inside the chroot and every Android binary run there sees zero properties.
$BB mkdir -p $A/dev/__properties__
$BB mount -o bind /dev/__properties__ $A/dev/__properties__ 2>/dev/null
echo "= chroot props: $($BB ls $A/dev/__properties__ | $BB wc -l)"

# --- WiFi -------------------------------------------------------------------
#
# Order matters more than anything else here.
#
# wmt_loader reads persist.mtk.wcn.combo.chipid. If that property is already set
# it assumes detection has happened and exits without ever opening /dev/wmtdetect
# - so restoring Android's property area BEFORE this point silently prevents the
# chip from ever being detected. Run the loader with no property area, let it
# read the chip id from hardware (ioctl on /dev/wmtdetect returns 0x6580), then
# mount the properties for everything that comes after.
#
# wmt_drv also registers stpwmt (major 190) only *after* successful detection,
# and there is no devtmpfs, so the node has to be made by hand afterwards.
# Vendor .ko blobs are pinned to the kernel they were built against: the CRC in
# module_layout comes from that build, so they load only into Sanytron's own
# binary. On a kernel we build ourselves wmt_drv is refused with "disagrees
# about version of symbol module_layout", and everything below then waits on a
# chip that will never answer - wmt_loader, then a twenty second poll for "set
# STP mode success". Probe once here so a from-source kernel still boots to a
# usable Couch, without a radio, instead of stalling in this block.
$BB insmod /vendor/lib/modules/wmt_drv.ko 2>/tmp/wmt_drv.err
# Either the vendor module loaded, or the driver is built into the kernel. A
# from-source kernel takes the second path: the vendor blobs are pinned to
# Sanytron's build and cannot load, so the connectivity driver is compiled in
# instead, and a built-in driver never appears in /proc/modules. It does
# register its character devices, so look for those too.
if $BB grep -q "^wmt_drv " /proc/modules ||
   $BB grep -qE "mtk_wcn_detect|wmtdetect|stpwmt|wmtWifi" /proc/devices; then
    WIFI=1
    echo "= wmt: $($BB grep -q "^wmt_drv " /proc/modules && echo module || echo built-in)"
else
    WIFI=0
    echo "= wifi SKIPPED: $($BB head -1 /tmp/wmt_drv.err 2>/dev/null)"
    mark $((BASE+2)) "S2 wifi skipped: no wmt driver (module or built-in)"
fi

if [ "$WIFI" = "1" ]; then
    $BB umount /dev/__properties__ 2>/dev/null
    $BB umount $A/dev/__properties__ 2>/dev/null

    # userdata now holds our rootfs, so there is no Android /data to borrow.
    # The wlan driver logs an NVRAM warning without it and works regardless.

    $BB sleep 1; $BB mdev -s

    # The connectivity driver loads its patches with filp_open() on a bare
    # filename ("ROMv2_lm_patch_1_1_hdr.bin"), not request_firmware, so
    # firmware_class.path does not apply and a kernel thread resolves it
    # against /. The vendor's wmt_launcher is supposed to hand the driver a
    # folder over an ioctl, but our built-in driver never receives it and falls
    # back to "use default patch name". Link the firmware into / so the bare
    # names resolve; without this the chip powers up, fails patch download and
    # WiFi never comes on.
    for f in /vendor/firmware/*; do
        [ -e "$f" ] && $BB ln -sf "$f" / 2>/dev/null
    done
    LD_LIBRARY_PATH=/system/lib:/vendor/lib /vendor/bin/wmt_loader >/tmp/wmt.log 2>&1
    echo "= wmt_loader rc=$? chip=$($BB grep -oE '190 mtk_stp_wmt' /proc/devices || echo NOT-DETECTED)"

    # stpwmt exists only once detection succeeded; no devtmpfs, so mknod it.
    [ -e /dev/stpwmt ] || $BB mknod /dev/stpwmt c 190 0 2>/dev/null

    $BB insmod /vendor/lib/modules/wmt_chrdev_wifi.ko 2>/dev/null
    $BB insmod /vendor/lib/modules/wlan_drv.ko 2>/dev/null
    $BB mdev -s
    [ -e /dev/wmtWifi ] || $BB mknod /dev/wmtWifi c 153 0 2>/dev/null

    # No property area is restored. The snapshot was device-specific (it carries
    # this unit's serial number among other things) and cannot ship in a
    # distributable image; it also broke detection above. Restored here only if
    # a snapshot happens to be present, which it is not by default.
    PROPS=""
    [ -f "$BUNDLE/props.tar.gz" ] && PROPS="$BUNDLE/props.tar.gz"
    [ -z "$PROPS" ] && [ -f /extra/props.tar.gz ] && PROPS=/extra/props.tar.gz
    if [ -n "$PROPS" ]; then
        $BB mount -t tmpfs tmpfs /dev/__properties__ 2>/dev/null
        ( cd /dev/__properties__ && $BB tar xzf "$PROPS" 2>/dev/null )
        $BB mkdir -p $A/dev/__properties__
        $BB mount -o bind /dev/__properties__ $A/dev/__properties__ 2>/dev/null
    fi

    # The launcher waits for Android properties even after hardware detection.
    # Share the installer bridge; never load it into wmt_loader itself.
    WMT_BRIDGE="$(dirname "$0")/couch-wmt-properties.so"
    LD_PRELOAD="$WMT_BRIDGE" \
    LD_LIBRARY_PATH=/system/lib:/vendor/lib:/system/lib/hw:/vendor/lib/hw \
        /vendor/bin/wmt_launcher -p /vendor/firmware/ >/tmp/wl.log 2>&1 &
    # Wait for the launcher to actually configure the chip rather than guessing:
    # powering on WiFi before the transport is set gives pwr_on fail(-3). The
    # vendor module logs "set STP mode success" at that point; the from-source
    # driver logs the same ioctl as "wmt_lib_set_hif:new hifType", about a tenth
    # of a second after the launcher starts. Waiting on the vendor string alone
    # cost the built-in driver the whole 20s timeout on every boot.
    n=0
    while ! $BB dmesg | $BB grep -qE "set STP mode success|wmt_lib_set_hif:new hifType"; do
        [ $n -ge 20 ] && break
        $BB sleep 1; n=$((n+1))
    done
    echo "= launcher $($BB pidof wmt_launcher >/dev/null && echo running || echo dead), stp after ${n}s"
    mark $((BASE+5)) "S5 launcher=$($BB pidof wmt_launcher >/dev/null && echo up || echo dead) stp=${n}s"
    # The wlan driver logs through pr_debug, and the kernel has dynamic debug,
    # so every one of its messages is compiled in but switched off: a failed
    # probe says nothing at all. Open the tap for the probe and close it once
    # wlan0 exists, so a failure lands in dmesg without the driver narrating
    # every scan afterwards. Left open on failure, so a manual retry
    # (echo 0 > /dev/wmtWifi; echo 1 > /dev/wmtWifi) is heard too.
    $BB mount -t debugfs none /sys/kernel/debug 2>/dev/null
    echo 'file *gen2* +p' > /sys/kernel/debug/dynamic_debug/control 2>/dev/null

    # Without an NVRAM record the driver invents a MAC from 00:08:22 plus three
    # bytes of jiffies, so the address, and with it the DHCP lease, changes on
    # every boot. It refuses one from userspace (SIOCSIFHWADDR: Not supported).
    # It does read /data/nvram/APCFG/APRDEB/WIFI, a 512-byte record whose bytes
    # 4-9 are the MAC, so write one derived from the eMMC CID: locally
    # administered (02:), unique per unit, the same on every boot. Everything
    # else stays zero. The zero version words then fail the driver's NVRAM
    # version check exactly as a missing record does, so on the firmware side
    # (TX power, PHY parameters, domain) nothing changes from before.
    NV=/data/nvram/APCFG/APRDEB/WIFI
    ID=$($BB cat /sys/block/mmcblk0/device/cid 2>/dev/null)
    [ -n "$ID" ] || ID=$($BB sed -n 's/.*androidboot\.serialno=\([^ ]*\).*/\1/p' /proc/cmdline)
    if [ -n "$ID" ]; then
        H=$($BB echo -n "couch-wlan0-$ID" | $BB md5sum | $BB cut -c1-10)
        $BB mkdir -p "$(dirname $NV)"
        $BB dd if=/dev/zero of=$NV bs=512 count=1 2>/dev/null
        { $BB printf '\002'
          for i in 1 3 5 7 9; do
              $BB printf "\\$($BB printf '%03o' 0x$($BB echo "$H" | $BB cut -c$i-$((i+1))))"
          done
        } | $BB dd of=$NV bs=1 seek=4 conv=notrunc 2>/dev/null
        echo "= wlan mac 02:$($BB echo "$H" | $BB sed 's/../&:/g; s/:$//') from id $($BB echo "$ID" | $BB cut -c1-8)..."
    else
        echo "= wlan mac: no device id, leaving the driver's random one"
    fi

    $BB echo 1 > /dev/wmtWifi 2>/tmp/wifion.err
    $BB sleep 3; $BB mdev -s
    $BB ifconfig wlan0 up 2>/dev/null
    if [ -d /sys/class/net/wlan0 ]; then
        echo "= wlan0 UP $($BB cat /sys/class/net/wlan0/address)"
        echo 'file *gen2* -p' > /sys/kernel/debug/dynamic_debug/control 2>/dev/null
    else
        echo "= wlan0 MISSING  $($BB cat /tmp/wifion.err 2>/dev/null)"
        echo "= wlan probe log:"; $BB dmesg | $BB grep -E "wlan_gen2.*(ERROR|WARN)|WMT-FUNC" | $BB tail -6
    fi

    # Credentials come from Android's own store, read on the device at runtime,
    # so they never live in this repo or the boot image. userdata is f2fs.
    # Android's store is a bonus, not a requirement. After installing to
    # userdata it is gone, and gating on it skipped our own saved networks too -
    # sending a perfectly configured device to the setup portal. wifi-conf.sh
    # merges /opt/couch/networks.conf with the store and copes if either is
    # missing, so the only real precondition is having a radio.
    CFG=/mnt/data/misc/wifi/WifiConfigStore.xml
    if [ -d /sys/class/net/wlan0 ]; then
        # Every stored network, not just the first: the first entry is not
        # necessarily the one in range.
        $BB sh "$(dirname "$0")/wifi-conf.sh" "$CFG" /tmp/wpa.conf | $BB tail -1
        NETS=$($BB grep -c "^network=" /tmp/wpa.conf 2>/dev/null)
        echo "= known networks: ${NETS:-0}"

        $BB chroot $A /sbin/wpa_supplicant -i wlan0 \
            -Dnl80211 -c /tmp/wpa.conf -B >/tmp/wpa.log 2>&1

        # Wait for association rather than guessing: a fixed sleep runs dhcp
        # while still SCANNING and it fails for no visible reason. With nothing
        # to associate to there is nothing to wait for - go straight to setup.
        n=0
        while [ "${NETS:-0}" -gt 0 ] && [ $n -lt 30 ]; do
            ST=$($BB chroot $A /sbin/wpa_cli -p /tmp/wpa -i wlan0 status 2>/dev/null | $BB grep "^wpa_state=" | $BB cut -d= -f2)
            [ "$ST" = "COMPLETED" ] && break
            $BB sleep 1; n=$((n+1))
        done
        NET=$($BB chroot $A /sbin/wpa_cli -p /tmp/wpa -i wlan0 status 2>/dev/null | $BB grep "^ssid=" | $BB cut -d= -f2)
        echo "= assoc ${ST:-none} ${NET:+to \"$NET\"} after ${n}s"

        # Run dhcp inside the chroot: busybox udhcpc does nothing without
        # /usr/share/udhcpc/default.script, and Alpine ships a maintained one.
        # Run from the initramfs it takes the lease and never applies it, which
        # looks exactly like dhcp failing.
        if [ "$ST" = "COMPLETED" ]; then
            $BB chroot $A /sbin/udhcpc -i wlan0 -n -q -t 10 >/tmp/dhcp.log 2>&1
            IP=$($BB ifconfig wlan0 2>/dev/null | $BB sed -n 's/.*inet addr:\([0-9.]*\).*/\1/p')
            if [ -n "$IP" ]; then
                # Root telnet, no password, on the LAN. Fine on a bench, not
                # something to ship - sshd with an enrolled key is the shipped
                # remote path, so this stays behind the debug flag.
                [ -n "$COUCH_DEBUG" ] && $BB telnetd -l /bin/sh -p 23 2>/dev/null
                echo "= NETWORK UP  $IP"
            else
                echo "= dhcp FAILED $($BB tail -1 /tmp/dhcp.log 2>/dev/null)"
            fi
        fi
        mark $((BASE+6)) "S6 assoc=$ST ip=${IP:-none}"
    fi


else
    echo "= wifi parked"
fi
