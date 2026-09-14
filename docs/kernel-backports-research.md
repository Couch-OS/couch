# Kernel backports research: a newer Bluetooth core (and other drivers) on 3.18

Research only, 2026-09-14. Nothing was built, flashed or changed; this is a
read-only survey plus a proposed experiment. It answers one question - *can we
give the HA100 a Bluetooth core new enough that BlueZ exposes its advertising
API, without leaving the MediaTek 3.18.79 base* - and then checks whether the
same trick is worth anything for the rest of the hardware.

## Executive summary

- **The Linux [backports](https://backports.docs.kernel.org/) project can still
  target a 3.18 base, and it does carry Bluetooth - but not in the same
  release.** Bluetooth was deleted from backports on 2017-02-06 ("None of these
  are maintaining backports anymore"). The newest release that has both
  `net/bluetooth` and a documented floor at or below 3.18 is
  **`backports-4.4.2-1`** (Feb 2016), generated from **Linux 4.4.2**, README:
  *"for kernels 3.0 and above"*.
- **A 4.4 Bluetooth core is exactly enough.** MGMT `Read Advertising Features`
  (0x003D) and `Add Advertising` (0x003E) landed in **4.1**, multiple
  advertising instances in **4.2**, LE Secure Connections in **3.19** - all
  inside 4.4. Our own doc and the kernel tree's notes say "4.2" for Add
  Advertising; the correct answer is 4.1, and it does not change the
  conclusion.
- **bluetoothd 5.79 (what the dev remote runs) would then export
  `org.bluez.LEAdvertisingManager1`.** BlueZ registers that interface *inside*
  the reply callback for 0x003D; 3.18's mgmt stops at 0x0039, the command fails,
  and the interface is never registered. That is precisely what we observed.
- **The win is not cosmetic.** On 3.18 the kernel clears `HCI_LE_ADV` when a
  central connects and only re-enables advertising if the *mgmt* advertising
  flag is set - which `hcitool` never sets. That is the reason `couch-bt-hid`
  re-enables advertising on a 15 s timer, and why any bluetoothd action can
  silently overwrite our advertisement. A managed advertisement fixes both, and
  gives milestone 4 (per-activity bonds, directed advertising) a supported path.
- **The cost is a new kernel candidate.** The base must drop `CONFIG_BT` (so the
  backported modules own the symbols) and gain `CONFIG_CRYPTO_CMAC`, which
  changes the pinned config hash and forces another hardware validation round.
  The `.ko` files cannot ride in a runtime bundle - the updater allowlist takes
  only `couch-*` binaries and known assets - so they travel in the boot ramdisk
  next to `couch-bt-bridge`, pinned to that kernel by `MODVERSIONS` and vermagic.
- **Toolchain is not the problem.** The 4.4 backports line adds no `-std` flag
  and inherits the base kernel's `-std=gnu89`; it ships a `compiler-gcc5.h`
  shim, i.e. it is contemporary with our GCC 4.9. The 6.1 line, which forces
  `-std=gnu11`, is the one we cannot use - and it has no Bluetooth anyway.
- **Beyond Bluetooth there is nothing worth backporting.** MT6580 in mainline is
  a serial console and three dummy clocks; there is no mainline driver for the
  tlsc6x touchscreen, the MTK DISP/LCM display, MTK IR TX or the MT6350 PMIC and
  its battery stack. The only plausible items are the accelerometer (*if* the
  die matches a MiraMEMS part mainline knows) and, distantly, `gpio-ir-tx`.
- **Recommendation:** run the cheap compile-only gate first (build
  `backports-4.4.2-1` for ARM against `out-wake` in the `couch-kbuild`
  container). If it compiles, spend one kernel build and one dev-remote session
  proving `LEAdvertisingManager1` appears; only then decide whether to carry a
  2016 out-of-tree Bluetooth core forever. The fallback - an in-process raw-HCI
  socket plus re-advertise on the disconnect signal - captures most of the
  benefit for a day of userspace work and no kernel risk.

---

## 1. Backports: which releases still support a 3.18 base, and is Bluetooth in them

### Supported base-kernel floors, per release line

The project's own [Releases page](https://backports.docs.kernel.org/releases.html)
says: *"All these releases should work against the same kernel major version and
all older versions till 3.10 for backports versions till 5.10. Later versions of
backprots should work with kernel versions down to version 4.4."* (typo theirs).
The front page still claims "back to version 3.0" and is stale. The per-line
`README` in [backports.git](https://git.kernel.org/pub/scm/linux/kernel/git/backports/backports.git/)
is the ground truth - it always reads *"This package provides the latest Linux
kernel subsystem enhancements for kernels X and above."*

| release line | README floor | subsystems | Bluetooth? |
|---|---|---|---|
| `linux-3.18.y` (v3.18.1-1, Dec 2014) | 3.0 and above | Ethernet, Wireless, Bluetooth, NFC, 802.15.4, Media, Regulator | **yes** |
| `linux-4.4.y` (**v4.4.2-1**, Feb 2016) | **3.0 and above** | Ethernet, Wireless, **Bluetooth**, NFC, 802.15.4, Media, Regulator | **yes** |
| v4.14-rc2-1 (Sep 2017) | 3.0 and above | Wireless, NFC, WWAN | no |
| `linux-4.19.y` (…v4.19.237-1, Apr 2022) | 3.10 and above | Wireless, NFC, WWAN | no |
| `linux-5.10.y` (…**v5.10.168-1**, Feb 2023) | **3.10 and above** | Wireless, WWAN | no |
| v5.11.22-1 … v5.15.162-1 | 4.4 and above | Wireless, WWAN | no |
| `master` / v6.1.97-1 … v6.1.145-1 (Jul 2025) | 4.14 and above | Wireless, WWAN | no |

The floor bumps are explicit, dated commits: "nuke support for kernels < 3.0"
(2014-04-17), "no longer support kernels < 3.10" (2017-10-13), the
"Remove support for kernel smaller than 4.0 … 4.4" series (2021-10-18, five days
before the 5.11/5.12/5.13/5.14 tarballs), and "… 4.5 … 4.14" (2024-04-10).

### Bluetooth was removed in 2017

Commit [`5288ae70e383`](https://git.kernel.org/pub/scm/linux/kernel/git/backports/backports.git/commit/?id=5288ae70e38369137e011e3391cfd2fd7d31bd7c),
Johannes Berg, 2017-02-06, *"backports: remove media, bluetooth, ethernet and
6lowpan - None of these are maintaining backports anymore, so remove them for
now."* It deletes `include/net/bluetooth/`, `net/bluetooth/` and
`drivers/bluetooth/` from `copy-list`. Today's
[`copy-list`](https://git.kernel.org/pub/scm/linux/kernel/git/backports/backports.git/plain/copy-list)
has no Bluetooth entry and `local-symbols` has no `BT*` symbol. The
[documentation page](https://backports.docs.kernel.org/documentation.html)
still lists Bluetooth under "Backported Subsystems" and has been wrong for nine
years.

So the two properties are only available together in the old releases:

- newest release usable on a 3.18 base at all:
  [`backports-5.10.168-1`](https://cdn.kernel.org/pub/linux/kernel/projects/backports/stable/v5.10.168/backports-5.10.168-1.tar.xz)
  (Wi-Fi/WWAN only - irrelevant to us, our Wi-Fi is a vendor CONSYS driver);
- newest release usable on a 3.18 base **with Bluetooth**:
  [`backports-4.4.2-1`](https://cdn.kernel.org/pub/linux/kernel/projects/backports/stable/v4.4.2/backports-4.4.2-1.tar.xz)
  (verified present, 18 Feb 2016).

### What is actually in `backports-4.4.2-1`

Inspected directly from the tarball:

- `versions`: `BACKPORTED_KERNEL_VERSION="v4.4.2-0-g1cb8570"` - the sources are
  Linux 4.4.2.
- `net/bluetooth/`: `hci_core.c`, `hci_conn.c`, `hci_event.c`, `hci_sock.c`,
  **`hci_request.c`**, **`hci_debugfs.c`**, `l2cap_core.c`, `l2cap_sock.c`,
  `mgmt.c`, `mgmt_util.c`, `smp.c`, **`ecc.c`**, `selftest.c`, plus `rfcomm/`,
  `bnep/`, `cmtp/`, `hidp/`. (Our 3.18 tree has none of `hci_request.c`,
  `hci_debugfs.c`, `ecc.c`, `mgmt_util.c` - they are the 3.19-4.4 refactor and
  the LE Secure Connections crypto.)
- `drivers/bluetooth/`: **`hci_vhci.c`**, `btusb.c`, the whole `hci_uart`
  family, `btintel`/`btrtl`/`btqca`/`btbcm`.
- Wiring: `Kconfig.sources` → `source "$BACKPORT_DIR/net/bluetooth/Kconfig"`;
  `Makefile.kernel` → `obj-$(CPTCFG_BT) += net/bluetooth/` and
  `obj-$(CPTCFG_BT) += drivers/bluetooth/`; 45 `BT*` entries in
  `.local-symbols`, including `BT_HCIVHCI`, `BT_LE`, `BT_BREDR`.
- There is no `defconfigs/bluetooth`; the 23 shipped defconfigs are all
  Wi-Fi/NFC/media. A two-line `defconfigs/bluetooth` is the easiest way in
  (`make defconfig-bluetooth` reads `defconfigs/<name>`).

### What a 4.4 core buys us

| feature | first kernel | in 4.4? | in our 3.18? |
|---|---|---|---|
| MGMT `Read Advertising Features` 0x003D | 4.1 ([`d3d5305bfd1c`](https://github.com/torvalds/linux/commit/d3d5305bfd1cb48c8f44207abb567276a1e09cc7)) | yes | **no** (mgmt stops at `SET_PUBLIC_ADDRESS` 0x0039) |
| MGMT `Add`/`Remove Advertising` 0x003E/0x003F | 4.1 ([`24b4f38fc9eb`](https://github.com/torvalds/linux/commit/24b4f38fc9ebf93af223c67169a946d6baf9db61)) | yes | **no** |
| advertising instances (max 5) + rotation | 4.2 ([`fffd38bca51c`](https://github.com/torvalds/linux/commit/fffd38bca51c9a1c00508b754ab66edb6f39cf37)) | yes | no |
| LE Secure Connections (SMP P-256, numeric comparison) | 3.19 | yes | **no** - `smp.c` has no `smp_f4`/`smp_f5`, no `ecc.c` |
| LE privacy / RPA (`MGMT_OP_SET_PRIVACY`) | 3.15 | yes | **yes, already** |
| LE connection parameter update | 3.17 | yes | yes |
| LE Data Length Extension | 4.0 | yes | no |
| BT 5.0 extended advertising (HCI) | 4.19 | no | no |
| MGMT extended-advertising API (0x0054/0x0055) | 5.11 | no | no |

`MGMT_REVISION` is 7 in 3.18 and 10 in 4.4. Extended advertising is out of reach
either way and we do not need it: a BLE HID remote advertises a 31-byte ADV_IND.

### Would bluetoothd 5.7x expose `LEAdvertisingManager1` with it? Yes.

BlueZ 5.79 `src/advertising.c` sends `MGMT_OP_READ_ADV_FEATURES` unconditionally
for every LE adapter and calls `g_dbus_register_interface(... LE_ADVERTISING_MGR_IFACE ...)`
**inside** `read_adv_features_callback`, after an early `return` on error. On
3.18 the kernel answers *Unknown Command* (0x01), bluetoothd logs
`Failed to read advertising features` and the interface simply never appears -
which is exactly the asymmetry we saw on hardware: `GattManager1` present,
`LEAdvertisingManager1` absent. With a 4.4 core the command succeeds,
`SupportedInstances` reports 5, and `RegisterAdvertisement` works.

Two traps worth writing down:

- `Adapter1.Roles` lists `"peripheral"` on 3.18 already (it is derived from
  `MGMT_SETTING_ADVERTISING`, which has existed for years). It is **not**
  evidence that advertising registration will work.
- BlueZ's `src/` never uses the legacy `MGMT_OP_SET_ADVERTISING` (0x0029); only
  `btmgmt` and the test tools do. So there is no D-Bus fallback on 3.18 - and
  even `btmgmt advertising on` would only give a kernel-generated payload
  (flags, name, appearance), with no service UUID and no scan response, which is
  useless for a HID peripheral.

### Why this matters more than "a nicer API"

The 3.18 kernel actively fights raw-HCI advertising once bluetoothd is running:

- `hci_le_conn_complete_evt()` clears `HCI_LE_ADV` when a central connects (the
  controller stops advertising by itself), and `mgmt_reenable_advertising()`
  returns immediately unless the *mgmt* `HCI_ADVERTISING` flag is set - which
  `hcitool` never sets. Net effect: advertise by hand, accept one connection,
  and after the peer disconnects the remote is dark. This is the exact bug
  `couch-bt-hid` papers over with `readvertise` every 15 s
  (`clients/couch-bt-hid/src/main.rs`), and the "Re-advertise immediately on
  disconnect" checklist item in [bluetooth.md](bluetooth.md).
- `set_powered`, `set_connectable`, `set_discoverable`, `set_le`,
  `start_discovery` and the add/remove-device paths all call the kernel's own
  `enable_advertising()`/`disable_advertising()`, which rebuild the advertising
  data from the kernel's model and overwrite whatever our `hcitool cmd 0x08
  0x0008` put there.

A managed advertisement removes both, and gives directed advertising per bonded
central (milestone 4's activity switching) a supported implementation instead of
a race.

---

## 2. Toolchain: can `backports-4.4.2-1` be built with GCC 4.9?

**Probably yes, and nothing in the evidence says otherwise.** Verified from the
tarball and the project:

- **No C-standard override.** `Makefile.kernel` sets only `NOSTDINC_FLAGS`
  (shim include paths and `-include backport-include/backport/backport.h`); it
  adds no `-std=`. The backported code therefore compiles under the base
  kernel's `-std=gnu89`, which is what our 3.18 build uses and what Linux 4.4
  itself used. The `-std=gnu11` that would break GCC 4.9 was added only on
  `master` in July 2024, for the 6.1 line.
- **Era-contemporary.** `backport-include/linux/compiler-gcc5.h` exists in this
  tree: it was written when GCC 4.9/5 were the kernel compilers. Linux 4.4's own
  `Documentation/Changes` asks for GCC 3.2 or later.
- **No documented minimum compiler.** Backports documents only
  *"git, python patch and coccinelle"*, and those only for regenerating a tree
  with `gentree.py` - not for building a released tarball. `devel/ckmake` checks
  that `gcc` exists, with no version constraint. Our `couch-kbuild` container
  already carries the AOSP `arm-eabi-4.9`-sabermod prebuilt the base kernel was
  built with, which is also the compiler the modules must use (see §3).

**What 3.18 lacks and backports shims for us:** `compat/` carries
`backport-3.19.c`, `backport-4.0.c`, `backport-4.1.c`, `backport-4.2.c`,
`backport-4.3.c`, `backport-4.4.c` (plus everything older), and
`backport-include/` carries 113 header shims under `linux/` alone. Those are
compiled into `compat.ko` and force-included into every backported file. This is
the whole point of the project and it is the part we would otherwise write by
hand.

**What backports does *not* shim - base-kernel config:** `net/bluetooth/Kconfig`
in this release reads

```
depends on m
depends on NET && !S390
depends on RFKILL || !RFKILL
depends on CRC16
depends on CRYPTO
depends on CRYPTO_BLKCIPHER
depends on CRYPTO_AES
depends on CRYPTO_CMAC
depends on CRYPTO_ECB
depends on CRYPTO_SHA256
```

Against `kernel/couch-ha100.config` today: `CRC16`, `CRYPTO`,
`CRYPTO_BLKCIPHER`, `CRYPTO_AES`, `CRYPTO_ECB`, `CRYPTO_SHA256` are all `=y`;
**`CONFIG_CRYPTO_CMAC` is not set** and must be turned on in the base kernel.
`RFKILL` is off, which the `|| !RFKILL` clause permits. `depends on m` means the
backported Bluetooth can only ever be a module - it cannot be built into the
zImage.

**ARM/3.18 gotchas to expect:**

- The build is an external-module build against the base *output* tree:
  `make KLIB_BUILD=<O= dir>` runs `make -C $(KLIB_BUILD) kernelversion` and
  generates `Kconfig.versions` from it. The 4.4 line's version parser handles
  `3.x` (`seq 0 19`); the 6.1 line's starts at 4.0, which is the mechanical
  reason 6.1 cannot see a 3.18 base at all.
- `ARCH=arm CROSS_COMPILE=arm-eabi-` must be passed through to the backports
  make, and the compiler must be the same GCC 4.9 build, or `vermagic` and the
  `MODVERSIONS` CRCs will not match (see §3).
- Nobody has done this recently. Backports gained CI only in 2024, for `master`
  only, and the project has never run runtime tests at all
  (*"we do not have the resources for automatic runtime tests"*). The 4.4 line's
  last activity was 2016. Expect to fix a handful of compile errors yourself and
  to own the result; there is no upstream to report them to (last commit on
  `master` is 2025-07-14, single maintainer).

---

## 3. Integration with our flow

### Base kernel changes (this is the expensive part)

| symbol | today | needed | why |
|---|---|---|---|
| `CONFIG_BT` | `y` | **not set** | the built-in core exports `hci_register_dev` and friends; a backported `bluetooth.ko` exporting the same symbols will not load |
| `CONFIG_BT_HCIVHCI` | `y` | not set | comes from the backported `hci_vhci.ko` instead |
| `CONFIG_CRYPTO_CMAC` | not set | **`y`** | hard Kconfig dependency of the backported `CPTCFG_BT` |
| `CONFIG_MODULES`, `MODULE_UNLOAD`, `MODVERSIONS` | `y` | unchanged | already correct |
| `CONFIG_MODULE_SIG` | not set | unchanged | no signing to arrange |

That changes the effective config hash, so per
[kernel-release-candidate.md](kernel-release-candidate.md) this is a **new
candidate**, not a drop-in: `kernel/release-pin.json` must be re-pinned and the
hardware checks (keys, key backlight, wake-from-standby, IR, display, Wi-Fi,
Bluetooth) re-run. Note the trade: with `CONFIG_BT` off, a remote that has the
new kernel but not the modules has **no** Bluetooth at all, where today the
kernel alone is enough.

### Modules: what, where, and how they reach a remote

Backports would produce `compat.ko`, `bluetooth.ko` and `hci_vhci.ko`
(names unchanged from upstream; `CPTCFG_BT=m`, `CPTCFG_BT_HCIVHCI=m`). Size is
small: 3.18's built-in core is 289 KB text + 18 KB data, so a few hundred KB of
`.ko` - irrelevant next to a 7.2 MB zImage, a 1.7 MB ramdisk and a 16 MiB boot
partition with ~7 MB spare.

**They cannot ship in a runtime bundle.** `daemon/couch-updates/src/staging.rs`
(`allowed()`, line 60) accepts only the `REQUIRED` names, `fbcon`, top-level
`couch-*` executables, `www/…` assets and `licenses/*.txt`. A `.ko` path is
rejected and the whole bundle refused. The right home is therefore the **boot
ramdisk**, next to the Bluetooth binaries that already live there:
`tools/release/prepare_boot_candidates.py::clean_ramdisk` copies
`couch-bt-bridge` and `couch-bt-hid` into `extra/` and then asserts the payload
set exactly, so adding modules means extending that `expected` set (and the same
function feeds `prepare_public_boot.py`, so both paths get it at once). The
outer root survives at runtime - `/extra/*` is reachable from the init root
(`/proc/1/root/extra` from inside the Alpine chroot) - which is how
`couch_system::bluetooth::base()` already falls back to `/extra`.

This is also the *correct* home for a different reason: `MODVERSIONS=y` plus the
`LOCALVERSION="-g<commit>"` that `kernel/build.sh` sets means a module only
loads into the exact kernel build it was compiled against. Shipping modules in
the boot payload keeps them in lockstep with the zImage by construction, and a
runtime-only update can never desynchronise them.

### `couch-bt-bridge` and `/dev/vhci` keep working

`hci_vhci.c` barely changed between 3.18 and 5.15 (398 → 375 lines; the growth
is 6.x debugfs/AOSP-emulation work). Device creation is the same write-based
vendor-packet protocol in both: `[0xff, type]`, type `0x00`, which is what
`h4::vhci_create_primary()` sends (`clients/couch-bt/src/lib.rs`). `HCI_BREDR`
was renamed `HCI_PRIMARY` in 4.13 with the same value. So the bridge, its
framer, the ENOSPC backoff and the STP whole-chip-reset errno handling are all
unaffected - the change is strictly above `/dev/vhci`.

Two userspace edits are needed:

- `couch_system::ui_settings::bluetooth_available()` tests
  `/dev/vhci` + `/dev/stpbt`. With modules, `/dev/vhci` does not exist until
  something `insmod`s `hci_vhci.ko`, so either the toggle loads the modules
  first (`insmod /extra/compat.ko`, `bluetooth.ko`, `hci_vhci.ko`, idempotent,
  before the bridge) or `initramfs/init` loads them at boot. Toggle-time is
  better: it keeps boot cost at zero and matches the "radio is on exactly while
  the bridge holds `/dev/stpbt`" model.
- `couch-bt-hid` switches from `hcitool` (`hci()` /`start_advertising()`) to
  `LEAdvertisingManager1.RegisterAdvertisement` over zbus, exporting an
  `org.bluez.LEAdvertisement1` object, and drops the 15 s `readvertise` tick.
  If nothing else needs `hciconfig`/`hcitool`, `bluez-deprecated` can leave
  `tools/provision-alpine.sh` too.

### Alternatives considered

- **`gentree.py --integrate`** copies backports *into* a kernel tree so it
  builds as part of it. It does not escape the module requirement
  (`depends on m` is in the generated Kconfig) and it would park a 2016 fork of
  `net/bluetooth` inside `couch-kernel`, which is a larger thing to maintain
  than a tarball we rebuild on demand.
- **Cherry-pick just the advertising commits into 3.18.** Tempting (four
  commits for 4.1, a handful more for 4.2), but 3.18 has no `hci_request.c` -
  the file the series lives in was split out afterwards - so this is a hand-port
  of the mgmt/`hci_core` advertising state machine, not a rebase. More
  error-prone than taking the whole tested 4.4 core, and it still needs
  `CONFIG_BT` to move.
- **In-kernel STP HCI driver** (the existing milestone 5) is orthogonal: it
  replaces the bridge below `hci_dev`, and would have to be written against
  whichever core is in play. Do not mix the two changes.

---

## 4. Beyond Bluetooth: is anything else worth backporting?

Short answer: **no**. For MT6580, mainline is not a source we can draw from -
it is behind our vendor tree by roughly every driver that matters.

Mainline `arch/arm/boot/dts/mediatek/mt6580.dtsi` (added in 4.3) is, in full:
four Cortex-A7 nodes, three *dummy* fixed clocks, a timer, sysirq, the GIC and
two disabled UARTs. `"mediatek,mt6580"` is not even in
`arch/arm/mach-mediatek/mediatek.c`'s machine table. There is no MT6580 clock
driver, no pinctrl, no MMC compatible, no PMIC wrapper entry - and the PMIC is
the **MT6350**, which appears nowhere in `drivers/mfd`, `drivers/regulator` or
`drivers/soc/mediatek`. The contemporary MT6582, which people *are* actively
mainlining, is at the same depth ten years later and hands the display over as
`simple-framebuffer`.

| block | our driver | mainline equivalent | realistic? |
|---|---|---|---|
| touchscreen | `drivers/input/touchscreen/mediatek/couch_tlsc6x.c` | **none** - `tlsc6x` (Telink) has never been submitted | No backport. But the report format is byte-identical to FocalTech/EDT (event+X[11:8], X[7:0], ID+Y[11:8], Y[7:0], pressure, area; power reg `0xa5`, sleep `0x03`), so mainline `edt-ft5x06` may already speak it. A 355-line unsubmitted community `tlsc6x.c` exists (Spotify Car Thing). Neither is worth doing while ours works |
| matrix keypad | `drivers/input/keyboard/matrix_keypad.c` (in-tree 3.18) | same file, heavily rewritten: gpiod + device properties + `guard()`, platform_data removed (6.12) | Do not copy the file - it will not compile. Hand-apply the three real fixes if we see the symptoms: force rows to input during scan (`01c84b03d80a`, 6.2), settle time after enabling columns and detect-change-during-scan (`90a0a63451e4`, `353bdd7d1456`, 6.15/6.16). The `linux,wakeup` → `wakeup-source` rename is 4.3; on 3.18 keep `linux,wakeup`, as [ha100-kernel-review.md](ha100-kernel-review.md) §3 says. `mt6779-keypad.c` (5.18) is mt6779/mt6873 only |
| IR TX | `drivers/misc/mediatek/irtx/mt6580/couch_irtx.c` (MTK PWM + DMA) | **none** - mainline MediaTek IR is receive-only (`mtk-cir.c`, mt7622/mt7623). Generic options: `pwm-ir-tx.c` (4.14), `gpio-ir-tx.c` (4.14), `ir-spi.c` (4.11) | `pwm-ir-tx` is blocked twice over: `pwm-mediatek.c` has no mt6580/mt6577 entry and would need a clock driver that does not exist. `gpio-ir-tx` needs only a GPIO but bit-bangs the carrier with interrupts disabled - a bad trade on a PREEMPT UI device with a working DMA-driven driver. Keep ours |
| display | `drivers/misc/mediatek/video/mt6580/*` + `lcm/st7701s_wvga_dsi_vdo_boe_tn_tianxian.c` | `drm/mediatek` covers MT2701/2712/7623/8167/8173/8183/8186/8188/8192/8195/8365; DSI covers six of those. MT6580 is pre-MMSYS and absent. No mainline `mtkfb` exists at all | Not a "add a compatible" job - a new display-controller port. Out of scope |
| LEDs | `drivers/misc/mediatek/leds/leds_drv.c`, `leds/mt6580/leds.c` | generic `leds-*` classes exist but nothing for MTK's dispatch model | Our fix (named pinctrl init, validated dispatch) is board knowledge mainline does not have |
| battery / fuel gauge | `drivers/power/mediatek/battery_common.c` (5 185 lines) | **none**. `mt6360`/`mt6370` chargers and `mt6323`/`mt6359` AUXADCs are different, later PMICs; there is no generic MTK gauge | Nothing to take. The only mainline-shaped sketch (SoC AUXADC → `generic-adc-battery`) is strictly worse than what we have |
| motion sensor | `drivers/misc/couch_motion.c` (MiraMEMS **DA218B**, I²C2/0x27) | IIO has `da280.c` (DA217/DA226/DA280, 4.10), `da311.c` (4.10), `mc3230.c` (4.9) - all tiny, i2c-smbus only, no MT6580 dependency | The only genuinely portable item on the list, **if** the die reports an ID `da280.c` accepts. DA218B is not in its table, so expect to add an ID at minimum. Our driver already works and exposes what the lift-to-wake feature needs; this is a tidiness project, not a capability one |
| USB gadget (`ttyGS0`) | `CONFIG_USB_G_ANDROID=y` (out-of-tree Android composite gadget) | gadget **configfs** landed in 3.9 and `f_serial`/`u_serial` are already in our tree; post-3.18 changes are cosmetic or for SuperSpeed hardware we do not have | Nothing to backport. If the serial gadget ever needs deterministic ACM-vs-`gser` behaviour (relevant to the Windows `usbser` installer route), the move is off `g_android` and onto configfs - a userspace change on the kernel we already run |

The two upstream commits worth keeping in a back pocket if symptoms appear:
`f_serial: ensure gserial disconnected during unbind` (2022) and
`f_serial: add suspend resume callbacks` (2020).

---

## 5. Recommended plan

### The smallest experiment that proves the BLE win

Three gates, each cheap, each abandonable. Nothing ships until gate 3 passes.

**Gate 1 - does it even compile? (Ollie only, no device, ~1-2 h)**

1. On Ollie, outside both repositories:
   `mkdir -p ~/backports && cd ~/backports && curl -O https://cdn.kernel.org/pub/linux/kernel/projects/backports/stable/v4.4.2/backports-4.4.2-1.tar.xz && tar xf backports-4.4.2-1.tar.xz`
2. Add a two-line `defconfigs/bluetooth`:
   `CPTCFG_BT=m` and `CPTCFG_BT_HCIVHCI=m`.
3. Build inside the pinned container against the **existing** `out-wake` tree
   (which still has `CONFIG_BT=y`; that is fine for a compile test):
   ```sh
   docker run --rm --user "$(id -u):$(id -g)" \
     -v ~/couch-kernel/base:/src:ro -v ~/couch-kernel/out-wake:/out:ro \
     -v ~/backports/backports-4.4.2-1:/bp -w /bp couch-kbuild sh -ec '
       make KLIB_BUILD=/out ARCH=arm CROSS_COMPILE=arm-eabi- defconfig-bluetooth
       make KLIB_BUILD=/out ARCH=arm CROSS_COMPILE=arm-eabi- -j24'
   ```
   (`/out` needs to be writable or copied; `KLIB_BUILD` is read for `.config`,
   `Makefile` and `Module.symvers`, and the module build writes into `/bp`.)
   **Pass:** `compat.ko`, `net/bluetooth/bluetooth.ko` and
   `drivers/bluetooth/hci_vhci.ko` exist. **Fail fast** if GCC 4.9 chokes on the
   4.4 sources - that is the whole point of doing this first.

**Gate 2 - does a 4.4 core attach to our radio? (one kernel build + dev remote,
~half a day)**

4. Add a kernel profile fragment (or a branch of `kernel/couch-ha100.config`)
   with `# CONFIG_BT is not set`, `# CONFIG_BT_HCIVHCI is not set`,
   `CONFIG_CRYPTO_CMAC=y`; build `normal` into a scratch output directory. Do
   **not** re-pin `kernel/release-pin.json` yet.
5. Rebuild the backports modules against that output (vermagic and MODVERSIONS
   CRCs must come from the kernel that will actually run them).
6. Get it onto the dev remote the way `.150.dev` got there: a dev build carrying
   the new zImage as a boot payload, with the three `.ko` files copied to
   `/extra` in the boot ramdisk - or, faster for a throwaway test,
   `tools/flash-linux.sh` plus `scp` of the modules into the outer root's
   `/tmp`. `/opt/couch/boot/previous.img` is the rollback.
7. On the remote: `insmod compat.ko; insmod bluetooth.ko; insmod hci_vhci.ko`,
   confirm `/dev/vhci` appears, then bring the stack up exactly as the toggle
   does (bridge → dbus → bluetoothd → `couch-bt-hid`).
   **Pass:** `hci0` appears, `hciconfig hci0 up` works, `hcitool lescan` sees
   advertisers - i.e. the same acceptance the original spike used, so a
   regression is obvious.

**Gate 3 - the actual win (~half a day)**

8. `dbus-send --system --print-reply --dest=org.bluez /org/bluez/hci0 \
   org.freedesktop.DBus.Introspectable.Introspect | grep LEAdvertisingManager1`
   **Pass:** the interface is present, and its `SupportedInstances` is non-zero.
   (`bluetoothctl show` also lists the advertising info once it exists.)
9. Teach `couch-bt-hid` to export an `org.bluez.LEAdvertisement1` object and
   call `RegisterAdvertisement`, behind a flag so the raw-HCI path stays
   available. Re-pair the TV; send `vol+`.
10. **The acceptance that matters:** disconnect the TV and confirm the remote
    starts advertising again *immediately*, with no 15 s tick and no `hcitool`,
    and that the advertisement still carries the HID UUID and the name after
    bluetoothd has been poked (turn the adapter discoverable, or run a scan).
11. Only then: decide, re-pin the kernel, run the full hardware validation round,
    extend the ramdisk payload allowlist, and open the change as a normal
    candidate.

### Effort

- Gate 1: 1-2 hours, most of it fixing whatever GCC 4.9 complains about.
- Gate 2: half a day, including one kernel build and one boot-image install.
- Gate 3: half a day of Rust plus the pairing test.
- **The real cost is after gate 3:** another full kernel candidate validation
  (keys, key backlight, wake, IR, display, Wi-Fi, Bluetooth) and a permanent
  maintenance commitment to a 2016 out-of-tree Bluetooth core with no upstream.
  Budget that, not the three days.

### Risks

- **Whole-chip reset interplay.** The change is entirely above `/dev/vhci`, so
  the STP/WMT coupling that broke Wi-Fi in the first spike is untouched. But a
  4.4 core is chattier at bring-up (HCI Reset, LE Set Random Address, the
  privacy/RPA machinery) and drives the controller on its own schedule for
  advertising rotation. Keep the bridge's ENOSPC backoff and reset-errno
  handling, and re-run the Wi-Fi coexistence check (iperf3, BT on and idle)
  before believing the power numbers still hold.
- **Nobody has done this.** Backports has never run runtime tests; the 4.4 line
  has been untouched since 2016; ARM32 + a MediaTek ALPS 3.18 base is not a
  combination anyone has exercised. Treat a clean compile as weak evidence.
- **Kernel config regression surface.** Turning `CONFIG_BT` off touches a kernel
  that is currently the validated baseline for keys, wake, IR and display. The
  config delta is tiny but the validation cost is not.
- **Module/kernel pinning.** `MODVERSIONS` + `LOCALVERSION` mean a module built
  against the wrong kernel silently refuses to load. Shipping the modules in the
  boot payload is what keeps this honest; a `.ko` in a runtime bundle would be
  both rejected by the updater and wrong in principle.
- **Memory and boot time: not a concern.** ~961 MB total with ~915 MB available;
  the modules are a few hundred KB, loaded at toggle time, so boot is unchanged.
- **Security posture.** A 2016 Bluetooth core carries 2016 bugs (BlueBorne-era
  L2CAP/SMP fixes landed later). We expose no BR/EDR and no L2CAP servers beyond
  what bluetoothd opens, and the radio is off unless the toggle is on - but it
  is a real consideration, and cherry-picking later CVE fixes into a dead
  backports tree would be on us.

### Fallback (and it is a good one)

If gate 1 or 2 fails, or the maintenance cost looks worse than the benefit, keep
the 3.18 core and fix the two things that actually hurt, in userspace:

1. **Move raw HCI in-process.** Replace the `hcitool` shell-outs in
   `couch-bt-hid` with an `AF_BLUETOOTH`/`BTPROTO_HCI` raw socket. Removes a
   process spawn per advertising command and the `bluez-deprecated` dependency,
   and lets us read command-complete status instead of an exit code.
2. **Re-advertise on the disconnect *event*, not on a timer.** Watch
   `org.bluez.Device1.Connected` (or `InterfacesRemoved`) over the zbus
   connection we already have and re-issue LE Set Advertising Data + Enable
   immediately. That closes the 15 s hole, which is the single most visible
   symptom of the missing mgmt API.
3. **For milestone 4 switching**, directed advertising (`ADV_DIRECT_IND` with
   the bonded peer address) is expressible in raw HCI on 3.18 today; the kernel
   will not help, but it will not stop us either as long as we re-assert after
   every bluetoothd action.

That fallback is a day of work, has no kernel risk, and leaves the backports
route open. It is also the sensible thing to do *first* if the BLE remote needs
to be solid before the next release, regardless of what the experiment shows.

## Results (2026-09-14, branch `bluetooth-backports`)

All three gates passed on the dev remote the same evening the plan was written.

- **Gate 1.** `backports-4.4.2-1` builds against our tree with GCC 4.9 after one
  shim patch: `backport-include/linux/cred.h` redefines `current_user_ns()` as
  the pre-3.8 macro whenever the name is not a macro, and on 3.18 it is an
  inline function; the shim is now guarded on `LINUX_VERSION_CODE < 3.8`. The
  patch lives on Ollie at `~/backports/0001-cred-shim-3.18.patch`. `CPTCFG_BT`
  only appears in the backports config once the base carries
  `CONFIG_CRYPTO_CMAC=y`, so the module build is really a gate-2 step.
- **Gate 2.** Kernel candidate `out-bt44` (`CONFIG_BT` and `BT_HCIVHCI` off,
  `CRYPTO_CMAC=y`, zImage `d5ba1966`, config `f3d45767`) boots; the stripped
  modules (compat 19 KB, bluetooth 600 KB, hci_vhci 11 KB, vermagic
  `3.18.79-couch-normal-g06b21c74526c`) load from the boot ramdisk's `/extra`,
  `/dev/vhci` needs `mknod c 10 137` (mdev only runs at boot), the bridge opens
  the radio and `hci0` comes up. `couch_system::bluetooth` does the loading and
  the node at toggle time; `prepare_boot_candidates.clean_ramdisk` ships the
  three modules from `build/backports/` when present.
- **Gate 3.** bluetoothd 5.79 exports `org.bluez.LEAdvertisingManager1` with
  `SupportedInstances` 5. Five toggle cycles in a row came up clean with the
  stack on in about 1.5 s and no transport errors; Wi-Fi unaffected.
- **One gotcha.** About 300 ms after `BT_open` the MediaTek firmware raises an
  HCI Hardware Error (code 0x02) during the 4.4 core's own setup pass, and the
  4.4 core (unlike 3.18, which only logged it) resets the device. bluetoothd
  powering the adapter on inside that reset made every init command time out
  once. The service now waits 3 s after `hci0` appears when `hci_vhci` is a
  module. Finding which init command provokes the event is open.
- Shipped as `.152.dev` (runtime + boot payload) for the dev remote. Still to
  do before this can be a candidate: `couch-bt-hid` registering an
  `LEAdvertisement1` (in progress), the disconnect/re-advertise acceptance, a
  full hardware round, and re-pinning as a normal candidate.
- **Transport timing (the real fragility).** The MediaTek STP layer says
  "ready" before the firmware acknowledges frames; a frame written in that
  window times out at STP level and the driver escalates to a whole-chip reset
  (Wi-Fi drops with it). The 3.18 core never hit this because nothing was sent
  until bluetoothd powered the adapter seconds later; the 4.4 core sends its
  setup pass the instant the controller exists. `couch-bt-bridge` now waits
  after `BT_open` (2 s on the first open after boot, 600 ms after that), then
  probes with HCI Reset until a Command Complete returns, and only then creates
  the virtual controller. `couch-bt-hid` retries `RegisterApplication` on
  `org.bluez.Error.Busy` (bluetoothd resetting the adapter). With those, eight
  consecutive toggle cycles came up clean with `LEAdvertisingManager1`-managed
  advertising (`ActiveInstances` 1) and no chip reset.

## Outcome (2026-09-15): the in-kernel driver, on both cores

The transport trouble above was the userspace pump, not the radio. The vendor
tree has a Kconfig entry for "MTK BT driver for BlueZ" and the STP core's
BlueZ mode, but no driver, so Couch wrote one: `hci_stp` (couch-kernel branch
`couch-hci-stp`, about 290 lines, also copied to `kernel/backports/hci_stp.c`
for the backports build). It registers an `hci_dev`, powers the BT function
on and off in the adapter's own open and close, transmits through
`mtk_wcn_stp_send_data` with the STP tx-event callback for flow control, and
receives through the STP core's BlueZ-mode hook with its own H4 reassembly.
Built as a module, it is loaded when Bluetooth is toggled on and unloaded when
it is turned off; there is no bridge and no `/dev/vhci`.

- On the in-tree 3.18 core (`bluetooth-hci-stp`, `.153.dev`): first toggle
  after boot in 2 s, 15 of 15 cycles, zero STP timeouts, zero chip resets.
- On the backported 4.4 core (`bluetooth-backports`, `.154.dev`, the four
  modules in the ramdisk): first toggle in 6 s (3 s of it a settle that may
  be unnecessary now), 6 of 6 cycles, zero STP timeouts, zero chip resets, and
  not one hardware-error event; bluetoothd exports `LEAdvertisingManager1`
  (SupportedInstances 4) and `couch-bt-hid` runs a managed advertisement.

`kernel/backports/build.sh` reproduces the module build. Still to do before
a candidate: the disconnect/re-advertise acceptance with a real TV, a full
hardware round on the CMAC kernel, pushing `couch-hci-stp` to the kernel
repo, and folding the modules into `tools/build.sh` images.
