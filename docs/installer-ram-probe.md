# RAM-only installer transport probe

This is a private, read-only benchmark scaffold, not an installer. No physical
boot, FunctionFS enumeration or throughput result is established by packaging it.
It reuses the tested `ea122a39` kernel; kernel builds are unnecessary. Build real
artifacts on Ollie and keep their boot template and outputs outside Git.

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

The verified candidate BusyBox was checked under ARM QEMU on Ollie for `mount`,
`mkdir`, `kill`, `sleep`, `cat` and `sha256sum`. No Android/vendor runtime files,
WiFi credentials or device-specific properties are needed for this USB probe.

## Packaging

On Ollie, with repository sources and private inputs available:

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
