# HA100 input and display validation

## Keypad routing

On September 8, 2026, physical D-pad presses produced no evdev events on the
fresh diagnostic build. The GUI had opened `mt_gpio_kpd` and `mtk-kpd` correctly.
All four matrix interrupt counters were zero. GPIO debug output showed row
pins 6, 9, 11, and 12 in mode 0 despite unmasked EINT channels.

The MT6580 pin descriptors advertise a separate EINT function in mode 6.
`gpio_request()` selects GPIO mode, and the legacy EIC branch of
`mtk_gpio_to_irq()` previously returned an IRQ without selecting that function.
Changing only those four pin modes live restored the D-pad and center button,
confirmed by raw press/release events and GUI wake/navigation logs.

Kernel commit `fc3d0e69` selects the pin's advertised EINT function when the
pinctrl GPIO bank maps it to a legacy EIC interrupt. It propagates mux errors
and preserves routing for pins without a separate EINT function. No shared
device-tree overlay was changed. After reboot the user confirmed D-pad operation.

## Panel commands and timing

The user reported distortion despite a clean framebuffer capture. The adapted
Wiko panel driver still contained donor-phone initialization and timing.
The HA100 stock kernel and bootloader contain an identical production table:

- Uncompressed stock kernel: offset `0x115efe8`.
- Backed-up `lk.img`: offset `0x4673c` (read only).
- Format: 45 records, each one-byte command, one-byte count, 64-byte payload.
- Packed table SHA-256:
  `a4f796d22f4f7d66932a864ab29f516649f811eb5dde4de55a5a191a15cf1564`.

Disassembly of stock `lcm_get_params` at `0xc057bf6c` showed the same
`0x400`-byte parameter layout as our compiled driver. Production timing is
vertical 8/20/20 and horizontal 8/20/20 (sync/back/front), with PLL clock 162.
Reset delays are 20/40/180 ms. Suspend sends the existing sleep table, then
holds reset low. Commit `f332af05` restores these values and the exact command
table. Analog/GIP settings and delays were preserved, not tuned experimentally.

The image was written to p8 with a complete checksummed readback. After boot,
the user confirmed **screen and D-pad working**. The independent p9 recovery
partition still has MD5 `2ac16bf92bf9d92220b8af3f0ea46600`.

## Reproduction and remaining checks

`kernel/patches/ha100-input-display.patch` contains both commits for application
with `git am` after baseline `7a0e5e8f`. Build only on a dedicated Linux host using
`kernel/build.sh diagnostic` or `kernel/build.sh normal`. Each output directory
contains its effective config, compiler identity, symbol files, and manifest.
Five host tests cover boot image packing and configuration merging.

GUI logs and framebuffer screenshots cannot prove panel appearance: retain
physical confirmation in display tests. Repeated cold boot and screen-off/wake,
unplugged suspend, touch, IR, and audio remain separate validation gates.
