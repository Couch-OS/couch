#!/bin/sh
# Capture the panel as a PNG.
#
# The framebuffer is the ground truth for anything visual, and reading it beats
# asking someone to describe what they see. Note the byte order: couch-gui
# writes r,g,b,a because this panel reads the low byte as red, so the raw buffer
# is already RGBA and needs no swizzling here.
set -e
cd "$(dirname "$0")/.."
: "${COUCH_IP:?Set COUCH_IP to the device hostname or IP address}"
IP=$COUCH_IP
KEY=${COUCH_KEY:-$HOME/.ssh/couch_dev}
OUT=${1:-build/screen.png}
SSH="ssh -i $KEY -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"

mkdir -p "$(dirname "$OUT")"
$SSH root@"$IP" 'dd if=/dev/fb0 bs=1920 count=800 2>/dev/null' > build/fb.raw
python3 - "$OUT" <<'PY'
import sys
from PIL import Image
raw = open("build/fb.raw", "rb").read()
w, h = 480, 800
img = Image.frombytes("RGBA", (w, h), raw[:w * h * 4])
img.convert("RGB").save(sys.argv[1])
print(f"{sys.argv[1]}: {w}x{h}")
PY
