# GPU acceleration on the HA100

> **Status (2026-09-08): active investigation.** A headless vendor EGL/GLES2
> context and an EGLImage-backed gralloc render target now work on the custom
> kernel. Full-frame `glReadPixels` measured 204ms. Shared-buffer clear,
> completion, CPU lock, copy and pixel verification measured 9.6–10.6ms, but
> verification found stale pixels. Explicit legacy ION sync did not fix them.
> This is a diagnostic probe, not a production Slint GPU backend.

The panel work that hurts is full-screen image compositing: a crossfade between
two 480x800 backdrops costs **44-57ms per frame** on the CPU, in both LVGL and
Slint. Everything else fits comfortably in a 16ms budget. So this document is
about that one workload.

## What the hardware has

| piece | state |
|---|---|
| Mali-400 MP1 | present; kernel driver built in (`/dev/mali`, 854 symbols in kallsyms) |
| ION allocator | `/dev/ion` present |
| binder | `/dev/binder`, `/dev/hwbinder` present |
| DRM/KMS | **absent** - no `/dev/dri`, so Mesa/Lima is not an option |

[Lima](https://docs.mesa3d.org/drivers/lima.html) supports Mali-400, with its
kernel driver upstream since Linux 5.2. Kernel builds now work on a dedicated
Linux build host, but our 3.18 vendor tree lacks that DRM interface. Backporting Lima plus its
infrastructure is substantially larger than enabling a configuration option.
The working experiment uses the existing proprietary driver in a separate
bionic process; the musl Slint process still needs an integration bridge.

## What the stock firmware ships

From the device's own `vendor` (p14) and `system` (p21):

    /vendor/lib/hw/gralloc.mt6580.so        28,928
    /vendor/lib/hw/hwcomposer.mt6580.so    386,532
    /vendor/lib/hw/memtrack.mt6580.so       16,296
    /vendor/lib/egl/libGLES_mali.so      1,323,632
    /system/lib/egl/libGLES_android.so     108,452   (software fallback)

The dependency split is what decides the shape of any port. These are
`DT_NEEDED` entries read from the dynamic section (tools/elfdyn.py), not strings
scraped from the binaries - an earlier pass using `strings` reported
`libsec_mem.so` as a gralloc dependency, which the dynamic section shows it is
not.

    gralloc.mt6580.so     (12) liblog libcutils libion libhardware libion_mtk
                               libged libgralloc_extra libutils libc++ libc
                               libm libdl

    libGLES_mali.so       (13) libutils libcutils libhardware liblog libgpu_aux
                               libgralloc_extra libged libdpframework
                               libmtk_drvb libc++ libc libm libdl

    hwcomposer.mt6580.so  (28) the above plus libui libui_ext libsync libbwc
                               libbinder libpower libgui libm4u libhidlbase
                               libhwbinder libhidltransport libpq_prot
                               vendor.mediatek.hardware.pq@2.0_vendor
                               android.hardware.power@1.0
                               vendor.mediatek.hardware.power@1.1_vendor

Neither gralloc nor the GL driver needs binder, HIDL or libgui. hwcomposer needs
all three, plus two MediaTek vendor HIDL interfaces.

**Every one of those libraries is present on the device**, across `/system/lib`
and `/vendor/lib` - the closure is complete for both routes, with nothing
missing. Whatever else is hard about this, sourcing the blobs is not.

## Two possible routes

    Slint -> GLES2 -> libhybris -> gralloc + libGLES_mali -> kernel  (route A)
    Slint -> GLES2 -> libhybris -> gralloc + HWC + libGLES_mali      (route B)

**Route A (no HWC) is dead.** `tools/../src/fbprobe.c` answers it directly: the
module loads, identifies itself, and opens its allocator - but there is no
framebuffer device at all.

    ok   dlopen /mnt/v/lib/hw/gralloc.mt6580.so
    ok   HMI  id=gralloc name=Graphics Memory Allocator Module
    ok   gpu0 (allocator) rc=0 dev=0xb6ca9000
    FAIL fb (framebuffer device) rc=-22

-22 is EINVAL: this gralloc has no "fb" device to open. That matches Android 8,
which retired the framebuffer HAL in favour of hwcomposer. The `fb_post` string
in the binary is dead code.

Two things that probe did establish, both of them useful:

- **The allocator works.** gralloc can hand out graphics buffers, which is the
  half of the stack a GPU path actually needs.
- **libhybris is not required to run these blobs.** The probe is an ordinary
  bionic executable running natively under `/system/bin/linker` - this is an
  Android kernel, so they simply run. libhybris is only needed later to let a
  *musl* Rust binary link against them.

**Route A-prime** falls out of that, and is the interesting one: let the GPU
composite into a gralloc buffer and copy the result to `/dev/fb0` ourselves.
`libGLES_mali` advertises `EGL_ANDROID_image_native_buffer` and
`EGL_KHR_image_base`, so a gralloc buffer can be wrapped as an EGLImage and used
as an FBO render target - no window system, no hwcomposer, no binder. The final
copy costs 1.32ms at the 2.2 GB/s measured to the framebuffer, which is
affordable inside a 16ms budget when the compositing itself has become free.

**Route B (with HWC)** is the Halium/Ubuntu Touch shape and is known to work on
MediaTek parts, but it means bringing up binder, HIDL and enough of libgui for
`HWComposerNativeWindow`. Much larger surface area.

## Unknowns to resolve before committing

1. ~~Is `fb_post` functional?~~ **Answered: there is no fb device (EINVAL).**
   Route A is out; route A-prime or B.
2. Can EGL initialise with no window system, and can a gralloc buffer be bound
   as an FBO render target via `EGL_ANDROID_image_native_buffer`? This is the
   next probe, and it decides whether route A-prime works.
3. Does libhybris build against musl? It is normally built for glibc, Alpine does
   not package it, and it ships its own linker - so this is a real porting task,
   not a package install.
4. Slint's `FemtoVGRenderer` needs an EGL context supplied through a custom
   OpenGL context wrapper. Documented for embedded, but another integration.

## The cheaper alternative, first

NEON is unexploited and this is a Cortex-A7. LVGL ships NEON blenders, but
`lv_blend_neon.S` is written in GNU `as` macro syntax that LLVM's integrated
assembler rejects, and the NDK is LLVM-only. At roughly 130ns per composited
pixel we are plainly not using SIMD at all, so hand-porting those kernels (or
writing intrinsics) is a day of work against libhybris's weeks, and a 3-4x blend
speedup would bring the 50ms crossfade close to budget on its own.

Worth trying before committing to the Android graphics stack.

## Reproducing the shared-buffer probe

Build `src/eglprobe.c` with the Android ARMv7 NDK compiler (`-O3 -Wall -Wextra
-ldl`). Its `gralloc_abi.h` records the Android 8.1 32-bit ABI and asserts its
size. Run inside an isolated root with the original p21 mounted read-only at
`/system`, p14 read-only at `/vendor`, and `/dev`, `/proc`, `/sys` bind-mounted.
This avoids mixing the reduced Couch connectivity bundle with graphics blobs.
Do not replace the live system/vendor mounts. Unmount the probe root afterwards.

```
/probe /vendor/lib/egl/libGLES_mali.so --shared
/probe /vendor/lib/egl/libGLES_mali.so --shared --ion-sync
```

The probe allocates through gralloc, imports an
[EGL_ANDROID_image_native_buffer](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_image_native_buffer.txt),
renders alternating colours, waits for GPU completion, locks for CPU reading,
and verifies every pixel across 30 frames. It never writes the display or
invents physical addresses. The handle contains a gralloc metadata fd and an
ION dma-buf fd; these are distinct. Any pixel mismatch returns failure.

Clear-only throughput is not a textured Slint benchmark. The original batched
clear timing can coalesce work and must not be reported as actual GUI frame
latency. Resolve cache ownership/completion correctness, then benchmark textured
compositing and presentation before integrating a GPU renderer. The existing
software renderer remains the production path.
