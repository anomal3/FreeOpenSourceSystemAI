#!/bin/sh
# N5/N6: собрать servoprobe (Servo 0.8) под <арх>-unknown-freeos.
#
#   docker run --rm -v <проект>:/work -v fossai-servo:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/servo/builder/build-servoprobe.sh x86_64 [check]
#
# Второй аргумент `check` — только `cargo check` (список того, что не
# собирается). Готовая программа — build/linux/servoprobe-<арх>.
set -e
ARCH=${1:-x86_64}
MODE=${2:-build}
. /work/ports/servo/builder/freeos-env.sh "$ARCH"
export CARGO_TARGET_DIR=/cache/target-servo
. /work/ports/servo/builder/servo-env.sh

# SpiderMonkey — как у jsprobe (build-jsprobe.sh): копия mozjs_sys с ОС freeos.
MOZJS_SYS=mozjs_sys-153.6.0-1
sh /work/ports/servo/mozjs/prepare.sh
mkdir -p /cache/src
(cd /work/build/servo && tar --exclude=.git -cf - "$MOZJS_SYS") | (cd /cache/src && tar -xmf - --keep-newer-files 2>/dev/null || true)
echo "mozjs_sys = { path = \"/cache/src/$MOZJS_SYS\" }" >> /tmp/patches.toml
export MOZJS_FROM_SOURCE=1
# libc++ для mozjs_sys — его build.rs знает freeos (freeos.patch); общий
# CXXSTDLIB=c++ сломал бы хостовый C++ (glslopt у webrender).
export PKG_CONFIG_PATH=$TARGET_PKG_CONFIG_PATH PKG_CONFIG_LIBDIR=$TARGET_PKG_CONFIG_PATH
SR=$FREEOS_SYSROOT/$ARCH
case $ARCH in
    x86_64) CLANG_TRIPLE="x86_64-unknown-linux-elf -U__linux__ -U__linux -Ulinux -U__gnu_linux__ -U__unix__ -U__unix -Uunix" ;;
    *) CLANG_TRIPLE="$ARCH-unknown-none-elf" ;;
esac
eval "export BINDGEN_EXTRA_CLANG_ARGS_$KEY=\"--target=$CLANG_TRIPLE -D__freeos__=1 -D__FreeOS__=1 -D_POSIX_THREADS=1 -D_POSIX_MONOTONIC_CLOCK=200112L -D_GNU_SOURCE=1 -nostdlibinc -nostdinc++ -isystem $SR/include/c++/v1 -isystem $SR/include\""
# stylo и привязки скриптов генерируют код на python хоста.
export PYTHON3=python3

cd /work/ports/servo/servoprobe
if [ "$MODE" = check ]; then
    cargo check --release -Zbuild-std=std,panic_abort -Zjson-target-spec \
        --target "$FREEOS_TARGET_SPEC" --config /tmp/patches.toml 2>&1 | tee /cache/servo-check-$ARCH.txt
    exit 0
fi
freeos_build /work/ports/servo/servoprobe
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/servoprobe" "/work/build/linux/servoprobe-$ARCH"
ls -la "/work/build/linux/servoprobe-$ARCH"
