# HA100 runtime validation — 2026-09-08

The live normal image is `couch-stable.img`, SHA-256
`48278dc417dd188cc70afabac661039000717d477f07720a34f206f3639909a1`.
Kernel source is `15b9bb349413240abb5538db678a2f9a8c4653ac`; the normal profile
**disables experimental PWM IR**. Kernel compilation ran natively on a dedicated
Linux build host.
The same source revision was also tested with IR enabled, so identify artifacts
by their manifest/config/image hashes, not uname alone.

- Production panel timing and keypad EINT configuration were physically
  confirmed by the user. The screen remains undistorted after boot.
- Touch reports preserve complete evdev batches and two-finger slot ownership.
  Raw capture previously observed 14 contacts and 14 matching releases.
  Kernel report reads respect the eight-byte I2C FIFO and retry transient errors.
- The GUI consumes the first dim-screen touch to restore brightness, then allows
  the next contact to select. Full powerdown still requires a physical button.
  Final physical confirmation of this dim-touch change remains pending.
- GUI health gating passed: advancing local heartbeats clear the BCB; a later
  read at uptime 519s confirmed the BCB is zero. Eight Python boot/build tests
  and eighteen GUI unit tests pass. Networking is not a health prerequisite.
- Rescue p9 remains unchanged: full-partition MD5
  `2ac16bf92bf9d92220b8af3f0ea46600`.
- Experimental IR failed a four-byte zero-waveform write: the device hung and
  watchdog-rebooted. Clock ordering did not resolve it. No target-device IR
  control has been validated. The driver is disabled in normal builds, and
  stage2 removes the stale stock `/dev/irtx` node when it is absent.
- GPU EGLImage imports succeed, but shared-buffer tests detect stale pixels.
  No GPU presentation path is enabled in the GUI.

Full unplugged suspend, battery/thermal behavior, IR reception by a target, and
long-duration GPU/allocator stability are separate outstanding hardware tests.
Existing workspace formatting differences mean `cargo fmt --check` is not yet
clean; this work does not claim a repository-wide formatting pass.

## Dim-touch reset conflict

The failed dim-tap capture contained zero bytes and IRQ261 stayed at 41. GPIO4
was low, while the live DT's `rstoutput1` assigns GPIO4 to touchscreen reset.
The legacy GPIO LED fallback interpreted `button-backlight` data=4 as GPIO4;
the GUI's normal dim operation writes zero to this LED, holding touch in reset.
This also explains the failed I2C read and why a physical key restored touch.

Kernel `30c130c8adb586eb5802a0c79070cde4ea67fa5b` prevents that LED fallback
from driving GPIO4 when the HA100 TLSC6x driver is enabled. Reset remains owned
by the touchscreen driver; full panel powerdown still intentionally suspends it.
Actual button-light control needs the vendor-specific implementation and must
not be inferred from this DT value. The normal LCD brightness path is unchanged.

The replacement image is `couch-dim-touch.img`, SHA-256
`af24106bc951215e7979c0933c21a1e93530f857935be01d16d5aa1fd94d7f9d`.

Post-reboot validation passed: button-backlight requests 0, 255, and 0 each
left GPIO4 high. The GUI then dimmed naturally and the user confirmed tap wake.
The GUI logged `wake from dim on touch` with 1ms panel check and 2ms backlight
work. This supersedes the pending dim-touch confirmation above.

## Wi-Fi status reporting

The GUI runs outside Alpine, where `/sbin/wpa_cli` is absent. Signal/carrier
were correct, but the failed executable lookup produced an empty SSID and a
false disconnected label. A bounded background Unix-datagram client now reads
`STATUS` from the shared `/tmp/wpa/wlan0` socket. The settings name refreshes
once per second; a missing name alone no longer means a disconnected link.
A native ARM probe run from the GUI's root reported COMPLETED and a nonempty
SSID, and the user confirmed the network name is visible. Settings now show
actual RSSI in dBm instead of a bar-count fraction. Socket permissions/cleanup,
status parsing and invalid RSSI values have regression tests.

## Wi-Fi scan, test and save flow (September 8)

Settings → Wi-Fi → Change network now scans first, lists RSSI in dBm, and
supports manual hidden-SSID entry. Secured networks open the password keyboard;
open networks proceed to review. Test temporarily selects a separate supplicant
network, checks its network ID and SSID, and acquires an IP address. Only explicit
Save writes network blocks to Alpine's persistent `networks.conf`. The password
is passed to `wpa_passphrase` on stdin; only its derived key is persisted.
Cancellation, a failed test or the 60-second save deadline restores the previous
network and enabled flags. A private `/tmp` journal supports GUI-restart recovery.
The test proves association and DHCP, not Internet connectivity.

The remote displayed the scan list and accepted D-pad input into both the
selected-network password screen and hidden-network name screen. Screenshots:
ignored `build/wifi-{scan,password,hidden}-device.png`. Saved Wi-Fi and house
configuration checksums stayed unchanged. The MTK driver emits a zero-RSSI
diagnostic scan entry; invalid RSSI values are excluded from the list. Twenty-eight
GUI tests pass, including six network regressions. No candidate connection was
selected during device review; a real network change awaits physical validation.

## Black-screen wake: PWM enable cache (September 8)

A physical black screen survived backlight nudges, GUI restart and an explicit
FBIOBLANK power cycle while mtkfb reported Alive and the framebuffer still changed.
A reboot restored it. Inspection found that `ddp_pwm.c` cached PWM_EN independently
of brightness and retained that enable cache through power transitions.

A controlled fault reproduced the mechanism on `30c130c8`: while awake, clearing
only PWM_EN through `pwm_test:set:0,0,1`, then writing brightness 254/255, left
register +0x00 at 0 even though duty register +0x14 was `0x03ff03ff` (full brightness).
Writing 0/255 restored it. This proves the cache defect; no register snapshot was
captured during the original spontaneous failure, so its exact trigger is not
proven.

Kernel `0d6673cd8337` reconciles PWM_EN against hardware while clocks are on and
invalidates the MT6580 enable cache across power transitions. It built on a
dedicated Linux build host
from a clean commit. `build/couch-pwm-resume.img` SHA256:
`2573bf876c2000499caadaa23ee722e74dd8668b35d2d4947658999da6ba45ad`.
Boot-partition readback verified; boot health cleared the BCB at normal timeout.
Recovery p9 remains MD5 `2ac16bf92bf9d92220b8af3f0ea46600`.

The identical fault injection now restores PWM_EN=1 with ordinary brightness
writes. Three automated cycles with dim=3s/off=8s each reached full powerdown and
woke to Alive/PWM_EN=1/full duty. Panel resume/present took 459–461 ms and backlight
writes 2–3 ms. Normal 30s/300s timers were restored. Physical confirmation and a
longer normal-use soak remain separate from these register-level checks.
