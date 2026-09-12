# Boot wordmark

`couch.png` is the HA100's 480×800 boot splash: white `couch.` centered on
black, using the bundled Inter font at weight **800**, 88px size, optical
size 32, and **−2px letter spacing**. The generator retains font kerning and
supersamples the lettering for smooth edges.

Generate a device-specific logo image from a backup of its current `logo`
partition:

```sh
python3 -m venv build/wordmark-venv
build/wordmark-venv/bin/pip install Pillow fonttools brotli
build/wordmark-venv/bin/python tools/build-boot-wordmark.py \
  build/logo-backup.img build/logo-couch.img --preview build/couch-boot.png
```

Inspect the preview before installation. On this HA100, the logo partition
is `mmcblk0p11`, 8 MiB; confirm its `PARTNAME=logo` and size on the target
before writing. Back up the entire partition outside Git, upload and verify
the generated image, then write only that partition and verify a complete
readback. Never write `lk` or `preloader_*`.

The generator replaces frame 0 only. All 38 charging, battery and other
image streams are retained byte-for-byte, as are the vendor header fields
and partition size. HA100 full-screen frames use **BGRA8888**, despite some
older MediaTek devices using RGB565. No kernel rebuild is needed. The new
splash appears on the next boot; this does not change the running Slint UI.

Validation: generated-image round trip checked all unchanged streams and
new frame pixels; physical device partition readback matched the uploaded
image on 2026-09-09. Physical boot appearance still needs visual confirmation.

The early Linux framebuffer splash now uses this same wordmark. Regenerate
`src/logo.h` with `python3 tools/mklogo.py`, rebuild `fbcon`, and repack both boot
and recovery ramdisks. Updating the logo partition alone does not update the
framebuffer executable embedded in the ramdisks.

Both image builders call `tools/build-fbcon.sh`, which rebuilds when the renderer, logo header or font header changes. A failed compiler/strip step preserves the previous binary and stops packaging. Set `NDK` to the host Android NDK on Linux or macOS.
