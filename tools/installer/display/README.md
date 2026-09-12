# RAM installer screen

The static C renderer is separate from the installer service. It opens only fbdev
for pixels and fixed RAM status files; it has no partition writer, network client
or credential parser. The 480×800 layout mirrors host phases and measured
counters without reproducing a wide terminal. It uses Inter 500 text and the
`couch.` wordmark at weight 800 with -2px tracking. Font source/license:
`assets/inter/InterVariable.woff2` and `assets/inter/LICENSE.txt`.

## Events

Write one bounded ASCII line atomically to
`/tmp/couch-installer-display.state`:

```
v1 download userdata 2097152 4194304 1048576 connected none
```

Fields: version, phase, target, bytes completed, total bytes, bytes/second,
WiFi state and error code. Zero rate means unknown. No free-form text is accepted.
`display_event.py` defines the shared allowlists and converts the same counters
as host `PhaseProgress`; initial/replayed counters do not invent a transfer rate.

- Phases: `wait connect download verify write complete error backup`.
- Targets: `none boot recovery userdata logo odmdtbo ram proinfo nvram nvdata protect1 protect2`.
- WiFi: `waiting ready connecting connected failed`.
- Errors: `none network verify storage protocol`.

Only fixed labels are drawn. `ram` is explicitly labeled a RAM transfer test;
completion says transfer complete, not that an OS has booted successfully.
WiFi's existing `/tmp/couch-wifi.status` overrides the network row when available.
Invalid events are rejected. The display retains the last valid event and
redraws at most once per second, only on changes. Frame writes follow the proven
4096-byte fbcon publication path; measure its performance cost on real hardware.

## Build and offline preview

Build real device artifacts on a Linux ARM cross-build host. Generate `display_assets.h` using Python
with Pillow 11.3.0, fonttools 4.60.1 and brotli 1.1.0:

```sh
python make_assets.py --font /path/to/InterVariable.woff2 --output display_assets.h
gcc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -O2 progress.c -o preview
arm-linux-gnueabihf-gcc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -Os -static -s progress.c -o couch-installer-display
python test_renderer.py --renderer ./preview
./preview --ppm /private/mock.state /private/mock.ppm
```

Normal invocation takes no arguments and uses `/dev/fb0`. Require the observed
480×800, 32-bit framebuffer with valid stride/channel fields. Init should create
the framebuffer character node from sysfs, then start this binary after proc,
sysfs and RAM `/tmp` are ready. Keep diagnostic output in RAM. Pixel publication
does not start, stop or acknowledge an installer transaction.

No display claims are physically validated by an offline mock. Package only the
reviewed static binary in a new candidate; do not modify an active stage image.
