#!/bin/sh
# N4: собрать /bin/jsprobe (SpiderMonkey через mozjs) под <арх>-unknown-freeos.
#
#   docker run --rm -v <проект>:/work -v fossai-servo:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/servo/builder/build-jsprobe.sh x86_64
#
# Исходники mozjs_sys с правками freeos — build/servo/mozjs_sys-<версия>
# (ports/servo/mozjs/prepare.sh). Сборка SpiderMonkey идёт в томе /cache:
# на примонтированном с Windows каталоге она в разы медленнее.
# Готовая программа — build/linux/jsprobe-<арх>.
set -e
ARCH=${1:-x86_64}
[ $# -gt 0 ] && shift
MOZJS_SYS=mozjs_sys-153.6.0-1
. /work/ports/servo/builder/freeos-env.sh "$ARCH"
sh /work/ports/servo/mozjs/prepare.sh

mkdir -p /cache/src
# Копия исходников в том: только изменённые файлы (-u), без .git.
(cd /work/build/servo && tar --exclude=.git -cf - "$MOZJS_SYS") | (cd /cache/src && tar -xmf - --keep-newer-files 2>/dev/null || true)
# makefile.cargo перезапускает configure, только если сам configure новее
# config.status; правки *.configure/constants.py он не видит.
newer=$(find /cache/src/$MOZJS_SYS/mozjs/build /cache/src/$MOZJS_SYS/mozjs/python/mozbuild/mozbuild/configure \
    -newer /cache/src/$MOZJS_SYS/mozjs/js/src/configure \( -name '*.configure' -o -name '*.py' -o -name config.sub \) | head -1)
[ -n "$newer" ] && touch /cache/src/$MOZJS_SYS/mozjs/js/src/configure

cat > /tmp/mozjs.toml <<EOF
[patch.crates-io]
mozjs_sys = { path = "/cache/src/$MOZJS_SYS" }
EOF

export CARGO_TARGET_DIR=/cache/target
export MOZJS_FROM_SOURCE=1
export CXXSTDLIB=c++
# mozjs_sys просит только -lc++; за ней — libc++abi (new/delete, исключения,
# __cxa_*) и libunwind, как у freeos-cc (CXX_LIBRARIES).
export RUSTFLAGS="$RUSTFLAGS -C link-arg=-lc++ -C link-arg=-lc++abi -C link-arg=-lunwind"
# configure SpiderMonkey без --with-sysroot берёт pkg-config из окружения как
# есть: только .pc цели (zlib из sysroot), не хоста.
export PKG_CONFIG_PATH=$TARGET_PKG_CONFIG_PATH PKG_CONFIG_LIBDIR=$TARGET_PKG_CONFIG_PATH
# bindgen (libclang) не видит обёртку freeos-cc: те же триплет, макросы и
# заголовки, что она даёт C++ (crates/freeos-cc: compile_flags, CXX_FLAGS),
# и без /usr/include хоста.
SR=$FREEOS_SYSROOT/$ARCH
case $ARCH in
    x86_64) CLANG_TRIPLE="x86_64-unknown-linux-elf -U__linux__ -U__linux -Ulinux -U__gnu_linux__ -U__unix__ -U__unix -Uunix" ;;
    *) CLANG_TRIPLE="$ARCH-unknown-none-elf" ;;
esac
eval "export BINDGEN_EXTRA_CLANG_ARGS_$KEY=\"--target=$CLANG_TRIPLE -D__freeos__=1 -D__FreeOS__=1 -D_POSIX_THREADS=1 -D_POSIX_MONOTONIC_CLOCK=200112L -D_GNU_SOURCE=1 -nostdlibinc -nostdinc++ -isystem $SR/include/c++/v1 -isystem $SR/include\""

freeos_build /work/ports/servo/jsprobe --config /tmp/mozjs.toml "$@"
mkdir -p /work/build/linux
cp -f "$CARGO_TARGET_DIR/$TRIPLE/release/jsprobe" "/work/build/linux/jsprobe-$ARCH"
ls -la "/work/build/linux/jsprobe-$ARCH"
