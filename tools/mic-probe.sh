#!/bin/sh
# Find out whether this remote has a working microphone.
#
# Everything here runs on the device and writes only to /tmp there; the one
# thing that comes back is a WAV you can listen to. couch-mic contains no
# network code at all, so nothing it records can leave except by this script
# copying it.
#
# Run it, and make a noise - talk at the remote, tap its case - for the whole
# sweep. The whole point is to find which of the card's capture devices hears
# you, and silence proves nothing.
#
# The device must already be on WiFi with sshd running; see docs/webui.md for
# how a key gets enrolled. sshd lives inside the Alpine chroot, so /opt/couch
# in a session is /mnt/alpine/opt/couch as the initramfs sees it.
set -e
cd "$(dirname "$0")/.."

: "${COUCH_IP:?Set COUCH_IP to the device hostname or IP address}"
IP=$COUCH_IP
KEY=${COUCH_KEY:-$HOME/.ssh/couch_dev}
OUT=${1:-build/mic.wav}
SECONDS_TO_RECORD=${SECONDS_TO_RECORD:-5}
SSH="ssh -i $KEY -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
BIN=clients/target/armv7-unknown-linux-musleabihf/release/couch-mic

[ -x "$BIN" ] || tools/build-voice.sh
mkdir -p "$(dirname "$OUT")"

echo "= pushing couch-mic to $IP"
scp -q -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null "$BIN" root@"$IP":/tmp/couch-mic.new
$SSH root@"$IP" 'mv /tmp/couch-mic.new /tmp/couch-mic && chmod 755 /tmp/couch-mic'

echo
echo "= what the card is, and what each capture device will take"
$SSH root@"$IP" '/tmp/couch-mic list'

echo
echo "= the mixer controls that gate the analogue path"
$SSH root@"$IP" '/tmp/couch-mic controls'

echo
echo "= sweeping every capture device - MAKE A NOISE AT THE REMOTE NOW"
$SSH root@"$IP" '/tmp/couch-mic sweep'

echo
echo "= recording ${SECONDS_TO_RECORD}s from the default device - KEEP TALKING"
$SSH root@"$IP" "/tmp/couch-mic record -t $SECONDS_TO_RECORD -o /tmp/mic.wav" || true

echo
echo "= fetching /tmp/mic.wav"
scp -q -i "$KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null root@"$IP":/tmp/mic.wav "$OUT" 2>/dev/null \
    && echo "  $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes) - play it" \
    || echo "  nothing to fetch; the recording failed above"

echo
echo "If nothing heard anything, the analogue path is probably powered down."
echo "Try the route Android's own HAL uses, then sweep again:"
echo "  $SSH root@$IP '/tmp/couch-mic route --dry-run'"
echo "  $SSH root@$IP '/tmp/couch-mic route'"
echo "  $SSH root@$IP '/tmp/couch-mic sweep'"
