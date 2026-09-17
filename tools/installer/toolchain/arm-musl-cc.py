#!/usr/bin/env python3
"""C compiler companion for ARMv7 musl dependencies using Zig 0.15.2."""
import os
import sys

zig = os.environ.get("ZIG", "zig")
# cc-rs forwards Rust's target spelling and ring's generic ARMv7 flag, which
# Zig rejects. Pin the HA100's Cortex-A7 CPU and musl hard-float ABI instead.
args = [arg for arg in sys.argv[1:]
        if not arg.startswith("--target=") and arg != "-march=armv7-a"]
os.execvp(zig, [zig, "cc", "-target", "arm-linux-musleabihf", "-mcpu=cortex_a7", *args])
