#!/bin/busybox sh
# Fixed hardware transition invoked only by couch-system while it owns the radio.
# ap0 is the MT6580's separate AP personality; never change wlan0's interface type.
set -e
BB=/bin/busybox
A=/mnt/alpine
$BB chroot "$A" /bin/sh -c 'killall hostapd dnsmasq wpa_supplicant udhcpc 2>/dev/null || true'
$BB sleep 1
$BB chroot "$A" /sbin/ip addr flush dev ap0 || true
$BB chroot "$A" /sbin/ip link set ap0 down || true
$BB sh "$(dirname "$0")/wifi-conf.sh" /dev/null /tmp/wpa.conf >/dev/null
$BB rm -f /tmp/wpa/wlan0
$BB chroot "$A" /sbin/ip link set wlan0 up
$BB chroot "$A" /sbin/wpa_supplicant -i wlan0 -Dnl80211 -c /tmp/wpa.conf -B
