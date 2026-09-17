#!/usr/bin/env python3
"""C compiler companion for optional ARMv7 musl GUI dependencies (Zig 0.15.2)."""
import os
import sys

zig = os.environ.get("ZIG", "zig")
# cc-rs spells Rust's target triple in a form Zig does not accept. ring also
# forwards GCC's generic ARMv7 architecture flag; Zig 0.15.2 rejects that
# spelling as a CPU while this wrapper already pins the concrete Cortex-A7.
args = [
    arg
    for arg in sys.argv[1:]
    if not arg.startswith("--target=") and arg != "-march=armv7-a"
]
os.execvp(zig, [zig, "cc", "-target", "arm-linux-musleabihf", "-mcpu=cortex_a7", *args])
