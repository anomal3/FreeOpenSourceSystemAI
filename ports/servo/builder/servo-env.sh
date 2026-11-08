#!/bin/sh
# Окружение сборки C/C++-зависимостей Servo под <арх>-unknown-freeos (N4).
# Подключать после freeos-env.sh: `. servo-env.sh` (нужны $ARCH, $KEY).
# Общее для depsprobe и самого Servo — найдено на depsprobe (ROADMAP N4).

# Копии крейтов с правками FreeOS: aws-lc-sys (getentropy вместо
# /dev/urandom).
. /work/ports/servo/builder/patch-crates.sh

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
# SQLite: расширения грузятся через dlopen — у нас его нет. VFS — обычный
# «unix»: блокировки fcntl(F_SETLK) даёт ядро (08.11.2026, SYS_FLOCK).
export LIBSQLITE3_FLAGS="-USQLITE_ENABLE_LOAD_EXTENSION -DSQLITE_OMIT_LOAD_EXTENSION"
