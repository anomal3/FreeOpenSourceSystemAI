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
# Копии крейтов с правками FreeOS (aws-lc-sys: getentropy вместо /dev/urandom).
. /work/ports/servo/builder/patch-crates.sh

export CARGO_TARGET_DIR=/cache/target-deps
# Крейт `cc` для неизвестной ОС берёт libstdc++; у нас libc++ (+abi, unwind).
eval "export CXXSTDLIB_$KEY=c++"
export RUSTFLAGS="$RUSTFLAGS -C link-arg=-lc++ -C link-arg=-lc++abi -C link-arg=-lunwind"
# Строгий `-std=c11` (aws-lc) у picolibc прячет POSIX (sigaction…); на Linux
# aws-lc сам ставит _XOPEN_SOURCE=700, прочим ОС — только __EXTENSIONS__.
CFLAGS_T=-D_DEFAULT_SOURCE
# aws-lc на AArch64 узнаёт возможности процессора у ОС (getauxval у Linux,
# elf_aux_info у BSD); у нас такого нет — берём их из флагов компилятора
# (NEON; AES/SHA — программно, пока нет HWCAP от ядра).
[ "$ARCH" = aarch64 ] && CFLAGS_T="$CFLAGS_T -DOPENSSL_STATIC_ARMCAP"
eval "export CFLAGS_$KEY=\"$CFLAGS_T\""
# zlib из исходников крейта, без pkg-config.
export LIBZ_SYS_STATIC=1
# aws-lc: без jitter-энтропии (проба заголовков), случайность — getentropy().
export AWS_LC_SYS_NO_JITTER_ENTROPY=1
# SQLite: расширения грузятся через dlopen — у нас его нет.
export LIBSQLITE3_FLAGS="-USQLITE_ENABLE_LOAD_EXTENSION -DSQLITE_OMIT_LOAD_EXTENSION"

freeos_build /work/ports/servo/depsprobe "$@"
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/depsprobe" "/work/build/linux/depsprobe-$ARCH"
ls -la "/work/build/linux/depsprobe-$ARCH"
