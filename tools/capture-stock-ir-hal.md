# Offline stock IR HAL capture

`capture-stock-ir-hal.py` runs the private HA100 Android ARM32 consumerir ELF in
Unicorn on a Linux build host. It relocates the original code, calls exported HMI's module
open with `transmitter`, then calls its installed transmit callback. Guest
`open`, `ioctl`, and `write` are simulated; raw syscalls and unknown imports fail.
No Android boot, NDK, linker, or physical remote is needed.

Use an existing private Python environment with `pyelftools` and `unicorn`:

```sh
python tools/capture-stock-ir-hal.py /private/consumerir.mt6580.so \
  --output /private/ir-capture --frequency 38000 \
  --pattern 9000,4500,560,560,560,1690,560
COUCH_STOCK_IR_HAL=/private/consumerir.mt6580.so \
  python -m unittest discover -s tools/tests -p test_stock_ir_hal_capture.py -v
```

The harness simulates GET_SOLUTTION_TYPE (`0x80045201`) returning 1, the observed
HA100 value. `--mode 0` is the stock default; mode 1 is available for comparison.
Property lookups use the provided/default strings, logging is discarded, and
memory/math routines are emulated. This exercises the HAL's actual packing code;
it does not reproduce Android services, kernel timing, electrical routing, or
LED output. Only the documented open/transmit paths run, not ELF constructors.

## Findings on September 10

Private stock HAL SHA256:
`05658d1ca0c03a13f16f74eb554170cb597cb7b42d187b7981c10da6e1ced8c8`.
Three fixture tests passed on the Linux build host: deterministic execution/mode differences,
9-high/17-low carrier samples, and guest syscall rejection.

At 38 kHz the stock bitstream has 9 high samples followed by 17 low samples,
using the stock driver's 1 µs sample period (about 38.46 kHz). LG NEC address
04/command 02 full-frame capture produced 8572 bytes in mode 0, 8500 in mode 1.
The corresponding final words were 1 and 127: waveform tail bits, **not a duration
trailer**. Full+ditto captures likewise end in waveform data. The original kernel
copies all supplied words; its DMA size register is word-count-minus-one.

Do not send these bytes to Couch's current driver. Couch/Rust deliberately use a
matched ABI with roughly 8.77 µs samples at 38 kHz and an explicit duration trailer;
raw stock bytes would be stretched and have a waveform word stripped. A physical
stock-equivalent comparison requires either a separately reviewed timing/ABI
adaptation or a matched diagnostic kernel. No such transmission is performed by
this tool.

ABI references: Android 8.1 [consumerir.h](https://github.com/aosp-mirror/platform_hardware_libhardware/blob/android-8.1.0_r1/include/hardware/consumerir.h)
and [hardware.h](https://github.com/aosp-mirror/platform_hardware_libhardware/blob/android-8.1.0_r1/include/hardware/hardware.h).
Keep original binaries and captured outputs outside Git.
