# Сборка compiler-rt builtins для bare-metal мишеней FreeOS.
# Использование:  powershell -File build-builtins.ps1 aarch64-none-elf aarch64
#
# Две ловушки, из-за которых рецепт выглядит именно так:
#  1. compiler-rt проверяет флаги через execute_process со списком аргументов,
#     и путь "C:/Program Files/..." разваливается по пробелу. Поэтому здесь
#     короткие 8.3-имена (C:/PROGRA~1).
#  2. Каждый аргумент cmake обёрнут в кавычки: в PowerShell голый токен вида
#     -DFOO=$Var уходит в cmake буквально, без подстановки.
param(
    [Parameter(Mandatory = $true)][string]$Target,
    [Parameter(Mandatory = $true)][string]$Processor
)

$ErrorActionPreference = 'Stop'
$TC    = Split-Path -Parent $MyInvocation.MyCommand.Path
$LLVM  = 'C:/PROGRA~1/LLVM/bin'
$CMAKE = 'C:\Program Files\CMake\bin\cmake.exe'
$NINJA = "$TC\bin\ninja.exe"
$SRC   = "$TC/llvm-project/compiler-rt/lib/builtins"
$BLD   = "$TC/build-$Target"

$args = @(
    '-G', 'Ninja',
    '-S', "$SRC",
    '-B', "$BLD",
    "-DCMAKE_MAKE_PROGRAM=$NINJA",
    '-DCMAKE_BUILD_TYPE=Release',
    '-DCMAKE_SYSTEM_NAME=Generic',
    "-DCMAKE_SYSTEM_PROCESSOR=$Processor",
    "-DCMAKE_C_COMPILER=$LLVM/clang.exe",
    "-DCMAKE_ASM_COMPILER=$LLVM/clang.exe",
    "-DCMAKE_CXX_COMPILER=$LLVM/clang++.exe",
    "-DCMAKE_C_COMPILER_TARGET=$Target",
    "-DCMAKE_ASM_COMPILER_TARGET=$Target",
    "-DCMAKE_CXX_COMPILER_TARGET=$Target",
    "-DCMAKE_AR=$LLVM/llvm-ar.exe",
    "-DCMAKE_NM=$LLVM/llvm-nm.exe",
    "-DCMAKE_RANLIB=$LLVM/llvm-ranlib.exe",
    '-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY',
    '-DCOMPILER_RT_BAREMETAL_BUILD=ON',
    '-DCOMPILER_RT_OS_DIR=baremetal',
    '-DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON',
    '-DCOMPILER_RT_BUILD_BUILTINS=ON',
    # --target= прописан во флагах вручную: у clang под Windows
    # CMAKE_C_SIMULATE_ID = MSVC, и из-за этого cmake НЕ разворачивает
    # CMAKE_C_COMPILER_TARGET в ключ --target=. Без этого всё собиралось бы
    # под x86_64-pc-windows-msvc и падало на «unsupported option -fPIC».
    "-DCMAKE_C_FLAGS=--target=$Target -ffreestanding",
    "-DCMAKE_ASM_FLAGS=--target=$Target",
    "-DCMAKE_CXX_FLAGS=--target=$Target -ffreestanding"
)

& $CMAKE @args
if ($LASTEXITCODE -ne 0) { throw "cmake configure failed for $Target" }

& $NINJA -C $BLD -j 2
if ($LASTEXITCODE -ne 0) { throw "ninja build failed for $Target" }
