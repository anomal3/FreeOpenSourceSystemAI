#!/bin/sh
# Сборка compiler-rt builtins для bare-metal мишеней FreeOS на Linux — то же,
# что tools/build-builtins.ps1 на ПК (CI: .github/workflows/craft.yml).
#
#   tools/build-builtins.sh x86_64      # или aarch64
#
# Исходники — build/toolchain/llvm-project (compiler-rt, cmake, llvm/cmake той
# же версии, что clang). Готовое — build/toolchain/compiler-rt/<арх>-none-elf/
# libclang_rt.builtins.a: там его ждут `cargo xtask toolchain` и `sdk`.
set -e
ARCH=${1:?arch}
TARGET=$ARCH-none-elf
TC=$(cd "$(dirname "$0")/.." && pwd)/build/toolchain
SRC=$TC/llvm-project/compiler-rt/lib/builtins
BLD=$TC/build-$TARGET

cmake -G Ninja -S "$SRC" -B "$BLD" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_SYSTEM_NAME=Generic \
    -DCMAKE_SYSTEM_PROCESSOR="$ARCH" \
    -DCMAKE_C_COMPILER=clang \
    -DCMAKE_ASM_COMPILER=clang \
    -DCMAKE_CXX_COMPILER=clang++ \
    -DCMAKE_C_COMPILER_TARGET="$TARGET" \
    -DCMAKE_ASM_COMPILER_TARGET="$TARGET" \
    -DCMAKE_CXX_COMPILER_TARGET="$TARGET" \
    -DCMAKE_AR="$(command -v llvm-ar)" \
    -DCMAKE_NM="$(command -v llvm-nm)" \
    -DCMAKE_RANLIB="$(command -v llvm-ranlib)" \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
    -DCOMPILER_RT_BAREMETAL_BUILD=ON \
    -DCOMPILER_RT_OS_DIR=baremetal \
    -DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON \
    -DCOMPILER_RT_BUILD_BUILTINS=ON \
    "-DCMAKE_C_FLAGS=--target=$TARGET -ffreestanding" \
    "-DCMAKE_ASM_FLAGS=--target=$TARGET" \
    "-DCMAKE_CXX_FLAGS=--target=$TARGET -ffreestanding"
ninja -C "$BLD"
mkdir -p "$TC/compiler-rt/$TARGET"
cp -f "$BLD/lib/baremetal/libclang_rt.builtins-$ARCH.a" "$TC/compiler-rt/$TARGET/libclang_rt.builtins.a"
ls -la "$TC/compiler-rt/$TARGET/libclang_rt.builtins.a"
