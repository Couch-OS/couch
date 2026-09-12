#!/bin/busybox sh
# Add the SELinux property-context files to the bundle and retry the launcher.
#
# bionic's property system maps property names to SELinux contexts using
# plat_property_contexts and nonplat_property_contexts. Without them property
# access misbehaves and wmt_launcher spins in a 300ms poll loop, never issuing
# the STP-mode ioctl - so stp_init reports "no hif info!" and power-on fails.
BB=/bin/busybox
B=/mnt/alpine/opt/couch
export LD_LIBRARY_PATH=/system/lib:/vendor/lib:/system/lib/hw:/vendor/lib/hw

$BB mkdir -p /mnt/asys /mnt/aven
$BB mount -t ext4 -o ro /dev/mmcblk0p21 /mnt/asys 2>/dev/null
$BB mount -t ext4 -o ro /dev/mmcblk0p14 /mnt/aven 2>/dev/null

$BB mkdir -p $B/system/etc/selinux $B/vendor/etc/selinux
for f in plat_property_contexts plat_service_contexts plat_file_contexts; do
    [ -f /mnt/asys/etc/selinux/$f ] && $BB cp /mnt/asys/etc/selinux/$f $B/system/etc/selinux/ 2>/dev/null
done
for f in nonplat_property_contexts nonplat_service_contexts nonplat_file_contexts; do
    [ -f /mnt/aven/etc/selinux/$f ] && $BB cp /mnt/aven/etc/selinux/$f $B/vendor/etc/selinux/ 2>/dev/null
done
[ -f /mnt/asys/etc/ld.config.txt ] && $BB cp /mnt/asys/etc/ld.config.txt $B/system/etc/ 2>/dev/null
$BB umount /mnt/asys /mnt/aven 2>/dev/null

echo "copied into the bundle:"
$BB ls $B/system/etc/selinux/ 2>/dev/null | $BB tr '\n' ' '; echo
$BB ls $B/vendor/etc/selinux/ 2>/dev/null | $BB tr '\n' ' '; echo
echo "visible via the bundle mount: $($BB ls /system/etc/selinux/ 2>/dev/null | $BB wc -l) + $($BB ls /vendor/etc/selinux/ 2>/dev/null | $BB wc -l)"

$BB killall wmt_launcher 2>/dev/null; $BB sleep 1
$BB dmesg -c >/dev/null 2>&1
/vendor/bin/wmt_launcher -p /vendor/firmware/ >/tmp/wl3.log 2>&1 &
$BB sleep 8
echo "--- did it configure the transport? ---"
$BB dmesg | $BB grep -oE "STP mode[^]]{0,40}|HIF info[^]]{0,40}|wmt_lib[^]]{0,50}" | $BB head -6
echo "--- then power on wifi ---"
$BB echo 1 > /dev/wmtWifi 2>/tmp/w3.err; echo "write rc=$? $($BB cat /tmp/w3.err 2>/dev/null)"
$BB sleep 4; $BB mdev -s
echo "wlan0: $([ -d /sys/class/net/wlan0 ] && echo PRESENT || echo missing)"
$BB dmesg | $BB grep -oE "pwr_on fail\(-?[0-9]*\)|no hif info" | $BB head -3
