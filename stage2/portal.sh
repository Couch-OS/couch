#!/bin/busybox sh
# Setup hotspot + captive portal, for when no known network is reachable and
# there is nobody on the USB serial line to fix it.
#
# phy0 - the station wiphy behind wlan0 - does NOT support AP mode; it offers
# only IBSS/managed/P2P. Writing "A" to /dev/wmtWifi makes the MediaTek driver
# register a second wiphy (phy1) with an ap0 netdev, and that one does support
# AP. hostapd has to run there; pointed at wlan0 it dies with "nl80211: Could
# not configure driver mode". Runs inside the Alpine chroot because that is
# where hostapd, dnsmasq and the busybox httpd live.
BB=/bin/busybox
A=/mnt/alpine
AP_IF=ap0
AP_IP=192.168.4.1
AP_SSID=${AP_SSID:-Couch-Setup}

echo "= starting setup portal on $AP_SSID ($AP_IP)"
echo "$AP_SSID" > /tmp/portal.ssid          # couch-gui puts this in the QR
: > /tmp/couch.setup                        # ... and shows the setup screen

# Scan BEFORE switching to AP mode, so the portal has a list to show the moment
# it loads rather than making every user wait through a scan.
# "-c /dev/null" gives wpa_supplicant no ctrl_interface at all, so wpa_cli has
# nothing to attach to: the scan returned nothing and the portal showed "No
# networks found" with no hint as to why. It needs a real config with a control
# socket, at the same path stage2 uses.
printf 'ctrl_interface=/tmp/wpa\n' > $A/tmp/scan.conf
$BB chroot $A /bin/sh -c '
    pidof wpa_supplicant >/dev/null ||
        wpa_supplicant -i wlan0 -Dnl80211 -c /tmp/scan.conf -B >/dev/null 2>&1
    sleep 1
    wpa_cli -p /tmp/wpa -i wlan0 scan >/dev/null 2>&1
    sleep 5
    wpa_cli -p /tmp/wpa -i wlan0 scan_results 2>/dev/null
' > /tmp/scan.raw 2>/dev/null

"$(dirname "$0")/couch-system" scan-json < /tmp/scan.raw > /tmp/scan.json 2>/dev/null
echo "= cached $($BB grep -o "ssid" /tmp/scan.json | $BB wc -l) networks for the portal"

# The radio cannot be a station and an AP at once.
$BB chroot $A /bin/sh -c 'killall wpa_supplicant 2>/dev/null; killall udhcpc 2>/dev/null'
$BB sleep 1

# Ask the driver for its AP personality. The chip must already be powered on as
# a station for this to take: the write moves wlan mode 0 -> 1 and spawns ap0.
echo A > /dev/wmtWifi
i=0
while [ ! -d /sys/class/net/$AP_IF ] && [ $i -lt 15 ]; do $BB sleep 1; i=$((i+1)); done
if [ ! -d /sys/class/net/$AP_IF ]; then
    echo "= no $AP_IF after ${i}s - cannot host the setup network"
    exit 1
fi
echo "= $AP_IF up after ${i}s"

# The setup network is open. Typing a passphrase off a 480x800 panel to reach a
# setup page is the worst part of provisioning a device like this, and it buys
# nothing here: the only thing on the portal that grants lasting access is SSH
# key enrolment, and that already requires a button press on the remote itself
# (implemented by couch-system). Radio range gets you the setup page, not the device.

cat > $A/tmp/hostapd.conf <<CONF
interface=$AP_IF
driver=nl80211
ssid=$AP_SSID
hw_mode=g
channel=6
auth_algs=1
wmm_enabled=0
ignore_broadcast_ssid=0
CONF

# busybox httpd serves the portal for any path via the 404 handler, so whatever
# URL the connectivity check asks for lands on the setup page.
WWW=$A/tmp/couch-portal-www
$BB mkdir -p "$WWW"
$BB cp -R "$(dirname "$0")/www/." "$WWW/" || exit 1
printf 'E404:/index.html\nI:index.html\n' > $WWW/httpd.conf

# Captive-portal detection: each OS fetches a fixed URL over plain HTTP and
# decides it is behind a portal from the answer. dnsmasq already resolves every
# hostname to us, so only the paths matter - but the 404 above is not good
# enough. Android counts 200-299 and 302/307 as "portal" and treats anything
# else, 404 included, as plain "no internet": it shows no sign-in prompt and may
# drop back to cellular. So these paths exist and answer 200 with a redirect to
# the real page. Apple's assistant opens its own sheet on any non-Success body,
# which then follows the same redirect.
$BB cat > $WWW/redirect.html <<HTML
<!doctype html><meta charset="utf-8">
<meta http-equiv="refresh" content="0;url=http://$AP_IP/">
<title>Wi-Fi Setup</title>
<p>Opening Wi-Fi setup&hellip; <a href="http://$AP_IP/">continue</a></p>
HTML
$BB mkdir -p $WWW/library/test
for probe in generate_204 gen_204 hotspot-detect.html connecttest.txt ncsi.txt \
             success.txt canonical.html nm-check.txt library/test/success.html; do
    $BB cp -f $WWW/redirect.html "$WWW/$probe"
done

# ap0 is created as type AP already, so no "iw set type" is needed (and it
# would fail: the driver owns this netdev).
$BB chroot $A /sbin/ip link set $AP_IF up 2>/dev/null
$BB chroot $A /sbin/ip addr flush dev $AP_IF 2>/dev/null
$BB chroot $A /sbin/ip addr add $AP_IP/24 dev $AP_IF 2>/dev/null

$BB chroot $A /usr/sbin/hostapd -B /tmp/hostapd.conf >/tmp/hostapd.log 2>&1 || exit 1
$BB sleep 2
# Hand out leases and answer every name with our own address, which is what
# makes the phone's connectivity check fail over into a captive-portal prompt.
cat > $A/tmp/dnsmasq.conf <<CONF
interface=$AP_IF
bind-interfaces
except-interface=lo
dhcp-range=192.168.4.10,192.168.4.100,255.255.255.0,12h
dhcp-option=3,$AP_IP
dhcp-option=6,$AP_IP
address=/#/$AP_IP
no-resolv
no-hosts
# RFC 8910: hand the portal URL straight to clients that understand the option,
# so they never have to infer it from a hijacked probe.
dhcp-option=114,http://$AP_IP/
CONF
$BB chroot $A /usr/sbin/dnsmasq -C /tmp/dnsmasq.conf >/tmp/dnsmasq2.log 2>&1 || exit 1
# Alpine's busybox is built WITHOUT the httpd applet - it lives in the
# busybox-extras package, which we do not ship - so "busybox httpd" there is a
# silent no-op and the portal serves nothing. Ours has httpd, so run our static
# binary from inside the chroot: the CGI scripts still want Alpine's /bin/sh.
# The copy MUST be called "busybox": it is a multi-call binary that picks its
# applet from argv[0], so invoked as "bb" it looks for an applet named bb and
# exits 127 with "applet not found".
$BB cp -f $BB $A/opt/couch/busybox 2>/dev/null
$BB chmod 755 $A/opt/couch/busybox 2>/dev/null
# Detach httpd's output: inheriting a console that can block (the USB serial
# tty in particular) wedges the CGI handlers and takes the shell with them.
$BB chroot $A /opt/couch/busybox httpd -p 80 -h /tmp/couch-portal-www \
    -c /tmp/couch-portal-www/httpd.conf >/dev/null 2>&1 || exit 1

# Report what is actually listening, not what we think we started: every one of
# these three has failed silently at some point.
$BB sleep 1
say() { $BB chroot $A /bin/sh -c "pidof $1 >/dev/null" && echo yes || echo no; }
WEB=$($BB netstat -tln 2>/dev/null | $BB grep -c ":80 ")
echo "= hostapd=$(say hostapd)  dnsmasq=$(say dnsmasq)  httpd=$([ "${WEB:-0}" -gt 0 ] && echo yes || echo no)"
# Put the credentials on the panel: this is the only place the passphrase is
# shown, and it is what ties setup to physical possession of the device.
{
    echo ""
    echo "=================================="
    echo "  WI-FI SETUP"
    echo "  join:  $AP_SSID   (no password)"
    echo "  then open:  http://$AP_IP/"
    echo "=================================="
    echo ""
} > /tmp/portal.banner
$BB cat /tmp/portal.banner

# The portal is the only way into a device with no known network, so it stays
# up until setup succeeds. PORTAL_TIMEOUT can still bound it for testing.
if [ -n "$PORTAL_TIMEOUT" ]; then
    ( $BB sleep "$PORTAL_TIMEOUT"
      $BB chroot $A /bin/sh -c 'killall hostapd dnsmasq 2>/dev/null'
      echo "= setup portal timed out" ) &
fi
