#!/bin/sh
# craftupd (обновление приложений Craft по подписанному индексу) под
# <арх>-unknown-freeos. В контейнере fossai-builder:
#
#   docker run --rm -v <проект>:/work -v fossai-craft:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/craft/builder/build-craftupd.sh [x86_64]
#
# Готовое — build/linux/craftupd-<арх> (в пакете craftupd, xtask craft).
set -e
ARCH=${1:-x86_64}
set -- "$ARCH"
. /work/ports/servo/builder/freeos-env.sh "$ARCH"
export CARGO_TARGET_DIR=/cache/target-craftupd
freeos_build /work/ports/craft/craftupd
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/craftupd" "/work/build/linux/craftupd-$ARCH"
ls -la "/work/build/linux/craftupd-$ARCH"
