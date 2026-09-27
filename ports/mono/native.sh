#!/bin/sh
# System.Native для Mono во FreeOS (фаза 60).
#
# Библиотеки классов Mono для Unix зовут часть своей работы через P/Invoke в
# `System.Native` — прослойку corefx на C (`external/corefx/src/Native/Unix/
# System.Native`). Без неё не работает даже `double.ToString()`: статический
# конструктор `Interop.Sys` первым делом зовёт `SystemNative_LChflagsCanSetHiddenFlag`.
# У Mono она собирается разделяемой библиотекой `libmono-native` и только для
# систем, которые configure знает поимённо; у нас разделяемых библиотек нет.
#
# Поэтому здесь — наш список файлов вместо их Makefile (как у Lua):
#   1. нужные файлы собираются нашим набором в libmono-native.a — с теми же
#      config.h и флагами, что вся Mono этой архитектуры;
#   2. по готовым объектникам составляется таблица экспортов (`freeos_exports`,
#      см. libc/freeos/include/dlfcn.h): каждая `SystemNative_*` — под именами
#      библиотеки `System.Native` и `mono-native`, как её зовут сборки и
#      настройки Mono. По ней `dlsym` libc и находит функции — Mono при этом не
#      меняется ни строкой;
#   3. Mono перекомпоновывается с таблицей и библиотекой.
#
# Сеть (`pal_networking`, `pal_maphardwaretype`: нет `net/if.h`, `AF_PACKET`) и
# учётные записи (`pal_uid`: у `struct passwd` picolibc нет `pw_gecos`) не
# собираются — сборка, позвавшая их функцию, получит
# `EntryPointNotFoundException` в этом месте, а не отказ при запуске.
#
# Использование (из корня репозитория, после configure и make этой архитектуры):
#   sh ports/mono/native.sh x86_64|aarch64
set -e
ARCH=${1:-x86_64}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
T=$ROOT/build/toolchain
export PATH="$T/bin:/c/Program Files/LLVM/bin:$PATH"
SRC=$T/thirdparty/mono-src/mono-6.14.1
B=$T/thirdparty/mono-$ARCH
N=$SRC/external/corefx/src/Native/Unix
OUT=$B/native-freeos
rm -rf "$OUT"; mkdir -p "$OUT"

# Флаги — те же, что у всей Mono (`-D_GNU_SOURCE` в том числе: по нему picolibc
# даёт GNU-вариант `strerror_r`, который config.h и объявил). Сверху: функции
# Annex K (`strcpy_s`, `memcpy_s`) picolibc объявляет только по просьбе, а
# config.h Mono их нашёл.
CFLAGS="-O2 -g -D_GNU_SOURCE -D_REENTRANT -DHAVE_CONFIG_H -D__STDC_WANT_LIB_EXT1__=1 -DHAVE_MEMCPY_S=1 \
  -I$B -I$SRC -I$SRC/mono -I$SRC/mono/native -I$N/Common -I$N/System.Native \
  -I$SRC/mono/eglib -I$B/mono/eglib -Wno-typedef-redefinition"

SOURCES="
$N/System.Native/pal_errno.c
$N/System.Native/pal_memory.c
$N/System.Native/pal_time.c
$N/System.Native/pal_io.c
$N/System.Native/pal_random.c
$SRC/mono/native/mono-native-platform.c
$SRC/mono/native/platform-type.c
"
for f in $SOURCES; do
  $ARCH-freeos-cc $CFLAGS -c "$f" -o "$OUT/$(basename "$f" .c).o"
done
$ARCH-freeos-ar rcs "$OUT/libmono-native.a" "$OUT"/*.o

# Таблица: всё, что объектники определяют под именем SystemNative_*.
{
  echo "/* Порождено ports/mono/native.sh — не править руками. */"
  echo "#include <dlfcn.h>"
  llvm-nm --defined-only -g "$OUT"/*.o | awk '$2 == "T" && $3 ~ /^SystemNative_/ { print $3 }' | sort -u > "$OUT/exports.txt"
  while read -r name; do echo "extern void $name(void);"; done < "$OUT/exports.txt"
  echo "const struct freeos_export freeos_exports[] = {"
  while read -r name; do
    echo "    {\"System.Native\", \"$name\", (void *)$name},"
    echo "    {\"mono-native\", \"$name\", (void *)$name},"
  done < "$OUT/exports.txt"
  echo "    {0, 0, 0},"
  echo "};"
} > "$OUT/freeos-exports.c"
$ARCH-freeos-cc -O2 -c "$OUT/freeos-exports.c" -o "$OUT/freeos-exports.o"
echo "System.Native: $(wc -l < "$OUT/exports.txt") functions -> $OUT"
