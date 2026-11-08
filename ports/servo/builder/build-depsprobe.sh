#!/bin/sh
# N4: собрать depsprobe (C/C++-зависимости Servo: HarfBuzz, FreeType+libpng,
# OTS, aws-lc, SQLite, zstd, zlib) под <арх>-unknown-freeos.
#
#   docker run --rm -v <проект>:/work -v fossai-servo:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/servo/builder/build-depsprobe.sh x86_64
#
# Готовая программа — build/linux/depsprobe-<арх> (xtask кладёт её в пакет servo).
set -e
ARCH=${1:-x86_64}
# dash не передаёт аргументы в `.` — `shift` только после freeos-env.sh.
. /work/ports/servo/builder/freeos-env.sh "$ARCH"
[ $# -gt 0 ] && shift
export CARGO_TARGET_DIR=/cache/target-deps
# Общие настройки C/C++-зависимостей Servo и копии крейтов с правками.
. /work/ports/servo/builder/servo-env.sh

freeos_build /work/ports/servo/depsprobe "$@"
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/depsprobe" "/work/build/linux/depsprobe-$ARCH"
ls -la "/work/build/linux/depsprobe-$ARCH"
