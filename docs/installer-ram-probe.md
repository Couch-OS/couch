# RAM-only installer transport probe

This is a private, read-only benchmark scaffold, not an installer. No physical
boot, FunctionFS enumeration or throughput result is established by packaging it.
It reuses the tested `ea122a39` kernel; kernel builds are unnecessary. Build ARM
artifacts on a Linux build host and keep boot templates and outputs outside Git.

## Isolated runtime

`tools/installer/probe/init` mounts only proc, sysfs, a RAM log directory and
FunctionFS. It does not run normal Couch init, stage2, boot-health logic, WiFi,
partition mounts or persistent markers. The cpio contains exactly three programs:
init, static ARM BusyBox and the actual static ARM probe service. Its sole block
node is `/dev/mmcblk0p9`, mode `0400`, for the service's read-only recovery hash.
This node permission is not a security sandbox: the root service must retain its
reviewed fixed-operation protocol and open storage read-only.

Legacy `android_usb` is configured for `ffs,acm`, with FunctionFS alias `couch`.
The service runs as `/bin/couch-installer-probe /dev/ffs-couch` and signals
`/tmp/couch-probe.ready` after registering descriptors. Init waits at most ten
seconds before enabling the gadget. If setup fails, ACM carries diagnostic
output only; it does not expose a command shell. Kernel configuration already
enables `CONFIG_USB_F_FS` and `CONFIG_USB_G_ANDROID`; the separate standalone
`CONFIG_USB_FUNCTIONFS` option is not required. Composite enumeration remains a
physical validation step.

The ACM node uses the `ttyGS` major observed in `/proc/devices` after gadget
configuration, rather than a stock hardcoded major. Init requests CPUs 1–3 online
as normal init does, and records available CPU online/governor/frequency values
in RAM for interpreting benchmark results. It does not change CPU governors.

Supply a static ARM BusyBox with `mount`, `mkdir`, `kill`, `sleep`, `cat`,
`sha256sum`, `awk`, `rm` and `mknod`. Verify these applets with ARM QEMU before
packaging; the builder checks ELF structure, not applet availability. No Android/vendor runtime files,
WiFi credentials or device-specific properties are needed for this USB probe.

## Packaging

On a Linux build host, with repository sources and private inputs available:

```sh
python3 tools/release/prepare_probe_ramdisk.py \
  --template /private/tested-boot.img \
  --kernel-manifest /private/board-init-manifest.json \
  --busybox /private/busybox-armv7l \
  --service /private/couch-installer-probe \
  --output /private/new-probe-directory
```

Missing or nonstatic executables are rejected; there is no placeholder service.
The builder validates the kernel provenance, checks the compressed ramdisk after
repacking and pads the image to exactly 16 MiB. `probe.json` records input and
image hashes with `installable: false` and physical-validation flags false.
The builder neither boots nor flashes anything. A controlled launch method and
recovery strategy must be reviewed separately; no RAM boot transport is implied.

Host regression checks:

```sh
python3 -m unittest discover -s tools/release -p 'test_prepare_probe_ramdisk.py'
sh -n tools/installer/probe/init
```

Measure RAM bulk transfer separately from device-local recovery hashing. Neither
test validates a future write protocol, image installation or power-loss recovery.

## Recovery-shell RAM upload

`tools/installer/serial_ram_upload.py` transfers a regular file of at most 64 MiB
to a generated `/tmp/couch-upload-…` path in an already running recovery serial
shell. This is separate from the probe’s output-only ACM diagnostics. The helper
requires `/tmp` to be tmpfs, uses exclusive/no-clobber creation, handles short
serial writes without duplicating data, and checks the received length and SHA-256.
It does not execute the uploaded file, boot an image or write a partition.

```sh
python3 tools/installer/serial_ram_upload.py \
  --port /dev/ttyACM0 --source /private/probe-payload --timeout 120
```

Choose the serial device explicitly and only when no other installer owns it.
After an interrupted transfer, the receiver state is ambiguous: independently
restart the recovery shell before sending another command or trying again.
The helper sends no automatic retry or reset. PTY tests validate framing and
checksum failure handling without contacting hardware.
