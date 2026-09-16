# Keypad acceptance tools

`keytap.c` counts presses and releases per key code from the `mt_gpio_kpd`
evdev node with kernel timestamps, so the keypad driver change (8 ms debounce,
50 us column settle, polling while a key is held; see
[the HA100 validation backlog](../../docs/ha100-validation-backlog.md)) can be checked
on hardware: `keytap --seconds 10`, tap DOWN 20 times, and the summary must
show 20 presses and 20 releases with no autorepeat events. `build.sh` makes a
static ARM binary in `build/keytap` with the Zig wrapper toolchain; copy it to
`/tmp` on the remote and run it from the root shell or the Alpine chroot.
While a key is held, `top` should show `kworker/0:1` idle rather than at ~20 %.
