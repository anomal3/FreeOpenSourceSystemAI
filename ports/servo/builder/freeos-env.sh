#!/bin/sh
# Окружение сборки под `<арх>-unknown-freeos` внутри контейнера fossai-builder.
# Подключать: `. /work/ports/servo/builder/freeos-env.sh x86_64` (или aarch64).
#
# Берёт то, что на ПК уже приготовил xtask (`cargo xtask test -s rust-std`):
#   build/toolchain/sysroot/<арх>   — libc, libfreeos, libc++, crt0, *.ld;
#   build/rust-std/src              — library/ с правками, libc и крейты
#                                     экосистемы (patches.toml — пути Windows,
#                                     здесь переписываются на /work).
# Обёртка freeos-cc собирается здесь под Linux (кеш — build/linux).
set -e
ARCH=${1:-x86_64}
W=/work
L=$W/build/linux

# Компилятор хоста (build-скрипты, host-часть configure) — clang образа.
mkdir -p /opt/freeos/bin
ln -sf "$(command -v clang)" /opt/freeos/bin/cc
ln -sf "$(command -v clang++)" /opt/freeos/bin/c++
export PATH=/opt/freeos/bin:$PATH
export HOST_CC=clang HOST_CXX=clang++
# rust-toolchain.toml проекта просит «nightly» — rustup взял бы свежий, а
# правки std привязаны к закреплённому (тот же, что на ПК).
export RUSTUP_TOOLCHAIN=nightly-2026-08-11

# freeos-cc под Linux, имена <арх>-freeos-<инструмент> — ссылками в /opt
# (bind-mount с Windows ссылок не держит).
CARGO_TARGET_DIR=$L/target-host cargo build -q --release --manifest-path $W/crates/freeos-cc/Cargo.toml --bin freeos-cc
mkdir -p /opt/freeos/bin
cp -f $L/target-host/release/freeos-cc /opt/freeos/bin/freeos-cc
for a in x86_64 aarch64; do
    for t in cc c++ ar ranlib nm objcopy strip ld; do
        ln -sf /opt/freeos/bin/freeos-cc /opt/freeos/bin/$a-freeos-$t
    done
done
export PATH=/opt/freeos/bin:$PATH
export FREEOS_SYSROOT=$W/build/toolchain/sysroot

TRIPLE=$ARCH-unknown-freeos
KEY=$(echo $TRIPLE | tr - _)
SR=$FREEOS_SYSROOT/$ARCH/lib
export FREEOS_TARGET_SPEC=$W/ports/rust-std/targets/$TRIPLE.json
export RUSTFLAGS="-L $SR -C link-arg=-T$SR/freeos-cxx.ld -C link-arg=$SR/crt0.o -C strip=debuginfo --cfg mio_unsupported_force_poll_poll"
export __CARGO_TESTS_ONLY_SRC_ROOT=$W/build/rust-std/src/library
export CARGO_TARGET_DIR=$L/target
eval "export CC_$KEY=$ARCH-freeos-cc CXX_$KEY=$ARCH-freeos-c++ AR_$KEY=llvm-ar"

# Подмены крейтов — те же, что у xtask, с путями контейнера.
sed 's#[A-Za-z]:/BlueGit/FreeOpenSourseSystemAI#/work#g' $W/build/rust-std/src/patches.toml > /tmp/patches.toml
# library/Cargo.toml указывает на libc относительным путём — менять нечего.

# Сборка: freeos_build <каталог крейта> [аргументы cargo]
freeos_build() {
    dir=$1; shift
    (cd "$dir" && cargo build --release -Zbuild-std=std,panic_abort -Zjson-target-spec \
        --target "$FREEOS_TARGET_SPEC" --config /tmp/patches.toml "$@")
}
echo "freeos-env: $TRIPLE, cc=$(command -v $ARCH-freeos-cc), sysroot=$FREEOS_SYSROOT/$ARCH"
