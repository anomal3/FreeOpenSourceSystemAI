#!/bin/sh
# Собрать OBSOS (браузер на Servo 0.8) и servoprobe под <арх>-unknown-freeos.
#
# Исходники OBSOS с 25.11.2026 — отдельный репозиторий
# https://github.com/anomal3/OBSOS (MIT OR Apache-2.0; на ПК Романа —
# E:\BlueGit\OBSOS, крейт obsos-fossai); его версия закреплена в
# ports/servo/OBSOS_REV.
#
#   docker run --rm -v <проект>:/work -v <OBSOS>:/obsos -v fossai-servo:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/servo/builder/build-servoprobe.sh x86_64 [check]
#
# Второй аргумент `check` — только `cargo check`. Готовые программы —
# build/linux/servo-browser-<арх> (в пакете /opt/obsos/bin/obsos) и
# build/linux/servoprobe-<арх>.
set -e
ARCH=${1:-x86_64}
MODE=${2:-build}
OBSOS=${OBSOS:-/obsos}
[ -f "$OBSOS/obsos-fossai/Cargo.toml" ] || { echo "нет OBSOS в $OBSOS: смонтируйте репозиторий OBSOS (-v <OBSOS>:/obsos)"; exit 1; }
WANT=$(tr -d ' \r\n' < /work/ports/servo/OBSOS_REV)
HAVE=$(git -C "$OBSOS" rev-parse HEAD 2>/dev/null || echo "?")
[ "$HAVE" = "$WANT" ] || echo "ВНИМАНИЕ: OBSOS на $HAVE, а закреплён $WANT (ports/servo/OBSOS_REV)"
# Правки OBSOS в крейтах Servo (vendor/) — хостом, до окружения FreeOS.
# Компоновщик хоста `cc` — clang образа (как в freeos-env.sh, тот ещё не подключён).
mkdir -p /opt/freeos/bin
ln -sf "$(command -v clang)" /opt/freeos/bin/cc
ln -sf "$(command -v clang++)" /opt/freeos/bin/c++
(cd "$OBSOS" && PATH=/opt/freeos/bin:$PATH RUSTUP_TOOLCHAIN=nightly-2026-08-11 cargo xtask vendor)
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
# Правки в крейтах самого Servo (gaol, шрифты, servo-script, -net, -paint,
# -storage) — в репозитории OBSOS ([patch.crates-io] его Cargo.toml, vendor/).
# stylo и привязки скриптов генерируют код на python хоста.
export PYTHON3=python3

cd "$OBSOS"
# Cargo.lock OBSOS собран для Windows/Linux: там libc/mio новее наших копий с
# правками FreeOS (patches.toml), и подмены остались бы неиспользованными
# (26.11.2026: libc 0.2.191 вместо 0.2.189 — нет clock_t/wchar_t у freeos).
# Свой Cargo.lock в кеше (resolver.lockfile-path): копия OBSOS с версиями
# подмен; репозиторий OBSOS не меняется. mio раньше libc — новый mio требует
# libc ≥ 0.2.190.
mkdir -p /cache/obsos-lock-$ARCH
LOCK=/cache/obsos-lock-$ARCH/Cargo.lock
cp -f "$OBSOS/Cargo.lock" "$LOCK"
LOCKCFG="resolver.lockfile-path=\"$LOCK\""
for pass in 1 2; do
    awk '/^\[\[patch.unused\]\]/{u=1;next} u&&/^name = /{n=$3} u&&/^version = /{print n, $3; u=0}' "$LOCK" | tr -d '"' |
    while read -r name version; do
        cargo update -q --config "$LOCKCFG" --config /tmp/patches.toml -p "$name" --precise "$version" 2>/dev/null || true
    done
done
if grep -q '^\[\[patch.unused\]\]' "$LOCK"; then
    echo "подмены не встали в Cargo.lock:"; grep -A2 '^\[\[patch.unused\]\]' "$LOCK"; exit 1
fi
if [ "$MODE" = check ]; then
    cargo check --release -p obsos-fossai -Zbuild-std=std,panic_abort -Zjson-target-spec \
        --target "$FREEOS_TARGET_SPEC" --config /tmp/patches.toml --config "$LOCKCFG" 2>&1 | tee /cache/servo-check-$ARCH.txt
    exit 0
fi
freeos_build "$OBSOS" -p obsos-fossai --config "$LOCKCFG"
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/servoprobe" "/work/build/linux/servoprobe-$ARCH"
# OBSOS на столе FossAi (obsos-fossai/src/bin/browser.rs) — /opt/obsos/bin/obsos.
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/browser" "/work/build/linux/servo-browser-$ARCH"
ls -la "/work/build/linux/servoprobe-$ARCH" "/work/build/linux/servo-browser-$ARCH"
