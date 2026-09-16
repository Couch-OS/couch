# Slint performance on the HA100

## Constraints and decisions (2026-09-08)

The Cortex-A7 has NEON/VFPv4 and a Mali-400 GPU. The panel uses MediaTek fbdev,
not DRM/KMS. Kernel builds run exclusively on a dedicated Linux build host; the Rust GUI is a static
ARMv7 musl executable and can be cross-compiled natively on the Mac.

- **CPU rendering:** retain cached RAM plus dirty-rectangle copies. This
  repository measured direct framebuffer rasterization at about 98ms versus
  about 1ms in cached RAM. Zero-copy into uncached display memory is therefore
  not inherently an optimization. Slint's [software renderer documentation](https://docs.slint.dev/latest/docs/rust/slint/platform/software_renderer/struct.SoftwareRenderer)
  recommends full buffers when RAM permits; line rendering is chiefly useful
  when full buffers do not fit.
- **GPU/shared buffers:** headless vendor EGL and gralloc EGLImage imports work.
  Readback measured 204ms; shared-buffer tests are faster but fail exact pixel
  verification. See [GPU investigation](gpu-acceleration.md). Do not enable the
  GPU in the GUI until ownership/cache correctness and textured rendering are
  validated. Clear-only timings do not measure Slint performance.
- **DMA blitting:** the vendor display has overlay/WDMA machinery, but this is
  not a generic userspace memcpy engine. A useful implementation needs owned
  DMA buffers, cache transitions, completion fences, and clipping. Slint 1.17's
  custom `TargetPixelBuffer` acceleration hooks are experimental. Neither raw
  physical addresses nor unsynchronized buffers belong in the GUI.
- **Scheduling:** normal kernel already enables preemption, high-resolution
  timers and tickless idle. Keep CFS and the RT bandwidth limits. GUI-wide FIFO
  priority can starve input/network/audio work; deadline scheduling needs a
  measured runtime budget and admission control, as explained in the
  [kernel documentation](https://docs.kernel.org/scheduler/sched-deadline.html).
  PREEMPT_RT or a modern scheduler is not a simple switch in this vendor 3.18
  tree. Screen-off input now blocks in `poll`, which wakes immediately for keys
  instead of polling every 40ms; local service work still runs every second.
- **Allocator:** `--features mimalloc` is an opt-in comparison, with the exact
  version recorded in `ui/Cargo.lock`. It is a userspace allocator, independent
  of the kernel slab allocator. [Mimalloc's bounded allocation claim](https://github.com/microsoft/mimalloc)
  explicitly excludes OS primitives; page faults and scheduling still matter.
  Retain musl by default until application measurements justify switching.

## Repeatable device comparison

Keep the same configuration/assets, charger state and governor for each run.
Build the historical baseline with `CARGO_PROFILE_RELEASE_OPT_LEVEL=s
RUSTFLAGS=''`. The default release build now targets Cortex-A7 at opt-level 3.
For an explicit candidate, use
`CARGO_PROFILE_RELEASE_OPT_LEVEL=3 RUSTFLAGS='-C target-cpu=cortex-a7' cargo build
--release --target armv7-unknown-linux-musleabihf` from `ui/`. Preserve each
binary under `/opt/couch/couch-gui-perf-*` before another build overwrites it.

After boot-health validation, run `sh /opt/couch/bench-gui.sh
couch-gui-perf-baseline 30`, then the candidate, then the baseline again. The
script temporarily pauses the supervisor, runs navigation/area transitions,
prints frame statistics/RSS/context switches, and restores the normal GUI on
exit. It changes the visible UI during the test. Compare rendering work
separately from intentional 60Hz pacing; do not call average frame costs
worst-case latency guarantees. Configuration with no multiple areas cannot
exercise area transitions.

### Optional allocator build

Install Zig 0.15.2 outside tracked source. The tested macOS ARM64 archive from
ziglang.org has SHA-256
`3cc2bab367e185cdfb27501c4b30b1b0653c28d9f73df8dc91488e66ece5fa6b`.
From `ui/`, export `ZIG` as the absolute executable path, then run:

```sh
cargo clean -p libmimalloc-sys --release --target armv7-unknown-linux-musleabihf
DEP_ATOMIC=c CC_armv7_unknown_linux_musleabihf="$PWD/../tools/arm-musl-cc.py" \
AR_armv7_unknown_linux_musleabihf="$ZIG ar" \
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 RUSTFLAGS='-C target-cpu=cortex-a7' \
cargo build --release --target armv7-unknown-linux-musleabihf --features mimalloc
```

`libmimalloc-sys` requests libatomic for every ARM target, although this
Cortex-A7 build inlines its atomics (verified with `llvm-nm -u` on
`libmimalloc.a`). `DEP_ATOMIC=c` uses its supported override; it does not supply
substitute atomic implementations. Its build script does not track that
variable, hence the targeted clean when changing it. The locked wrapper
includes mimalloc **3.3.2**, not the newest upstream release.

## Initial on-device results

Thirty-second navigation/area runs, USB connected, existing interactive governor
and three-core floor retained. Weighted mean is across reported five-second
windows, so these are not whole-run percentiles:

| Build | Frames reported | Mean work | Worst observed work | RSS |
|---|---:|---:|---:|---:|
| Size baseline, run 1 | 634 | 4.111ms | 20.763ms | 4,836KiB |
| Cortex-A7 / opt-level 3 | 637 | 3.761ms | 18.588ms | 5,368KiB |
| Size baseline, run 2 | 634 | 4.093ms | 20.716ms | 4,836KiB |

The CPU-tuned build is the default: roughly 8% lower rendering work at the cost
of about 0.5MiB additional resident memory and a 0.47MB larger executable.
Frame pacing remains 60Hz; this does not claim an 8% increase in visible FPS.
Physical input latency and full-screen media crossfades need separate validation.

Full logs and binary hashes are in [the benchmark record](performance/ha100-20260908.json).
- bench-candidate-2: 3.786ms mean work, 18.477ms worst observed, 5368KiB RSS.
- bench-mimalloc-1: 3.589ms mean work, 17.391ms worst observed, 5228KiB RSS.
- bench-mimalloc-2: 3.581ms mean work, 17.587ms worst observed, 5228KiB RSS.

Mimalloc remains opt-in: the modest additional gain merits longer voice/UI
soak testing before adding a C toolchain dependency to every deployment build.

## Cyrillic font comparison (2026-09-16)

Compared the bitmap-font Cyrillic build with SDF on the HA100, using the same
ARMv7 release profile and musl allocator. Each binary ran for 22 seconds with
`COUCH_DEMO=1 COUCH_NAV=1 COUCH_SLIDE=1`, followed by restoration of the installed
GUI. The device was on battery, with the interactive governor and cores 0–2
online; CPU frequency at the end of each run was 604.5MHz. Both builds used
the same bounded Cyrillic character set.

| Font build | Frames reported | Mean work | Worst observed work | RSS |
|---|---:|---:|---:|---:|
| Bitmap, run 1 | 543 | 3.869ms | 19.717ms | 13,268KiB |
| SDF | 545 | 4.072ms | 21.120ms | 12,088KiB |
| Bitmap, run 2 | 544 | 3.872ms | 19.943ms | 12,944KiB |

SDF added about 0.20ms (5.2%) to mean rendering work in this sample and used
856–1,180KiB less resident memory. The weighted means cover the reported
five-second windows, excluding deliberate frame pacing; these short runs do
not establish input latency or worst-case bounds. This tradeoff buys a smaller
executable and runtime package even compared with the original Latin-only
build; see [font size measurements and UI samples](slint-notes.md#glyph-embedding-happens-at-compile-time).

The production-font regression test also passed on the HA100, rendering all
76 seeded Cyrillic characters and the runtime temperature symbols in both
Lato weights. Headless production-UI samples cover small labels, thermostat
targets, pairing digits and keyboard text. Recheck appearance and timing when
changing the font inputs or Slint version.
