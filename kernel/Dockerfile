# Build environment for the MT6580 alps-3.18.79 kernel.
#
# amd64 because the AOSP arm-eabi-4.9 prebuilt (the toolchain config-stock.txt
# names via CROSS_COMPILE="arm-eabi-") is an x86_64 Linux binary.
# Build and run this container on any amd64 Linux builder with Docker access.
#
# ubuntu:20.04 because MediaTek's DrvGen.py - the DCT code generator the ALPS
# Makefiles run to produce the cust GPIO/EINT headers - is Python 2, which
# Debian bookworm no longer ships.
FROM --platform=linux/amd64 ubuntu:20.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential make bc bison flex libssl-dev libncurses-dev \
      git wget curl ca-certificates cpio kmod xz-utils zlib1g-dev \
      python2 python3 rsync file unzip \
 && rm -rf /var/lib/apt/lists/* \
 && ln -sf /usr/bin/python2 /usr/bin/python

# The era-correct cross compiler this kernel was built with (AOSP prebuilt,
# mirrored on GitHub since Google dropped the old GCC prebuilts from AOSP).
ARG TOOLCHAIN_COMMIT=031cb28febb0bb3f6a94a2a27141954a23e93eda
RUN git init /opt/arm-eabi-4.9 && \
    git -C /opt/arm-eabi-4.9 remote add origin https://github.com/crdroidandroid/android_prebuilts_gcc_linux-x86_arm_arm-eabi-4.9.git && \
    git -C /opt/arm-eabi-4.9 fetch --depth 1 origin "$TOOLCHAIN_COMMIT" && \
    git -C /opt/arm-eabi-4.9 checkout --detach FETCH_HEAD && \
    rm -rf /opt/arm-eabi-4.9/.git && \
    /opt/arm-eabi-4.9/bin/arm-eabi-gcc --version | head -1

ENV PATH=/opt/arm-eabi-4.9/bin:$PATH
ENV ARCH=arm CROSS_COMPILE=arm-eabi-
WORKDIR /src
