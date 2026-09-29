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
# Фаза 61b: в ту же таблицу — libgdiplus (если `ports/mono/gdiplus.sh` её
# собрал) и две функции libc, которые сборки зовут через `DllImport("libc")`;
# список библиотек для перекомпоновки mono — в native-freeos/libs. Фаза 62 —
# окна стола (`freeos`) для драйвера WinForms.
#
# Использование (из корня репозитория, после configure и make этой архитектуры
# и, для System.Drawing, после ports/mono/gdiplus.sh):
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

# Фаза 61b: libgdiplus (`ports/mono/gdiplus.sh`), если собрана, — её функции
# `Gdip*` идут в ту же таблицу под именем `gdiplus` (так их зовёт
# System.Drawing: `[DllImport("gdiplus")]`), а сама она и всё, на чём она
# стоит, — в перекомпоновку mono.
GDIPLUS=$B/gdiplus-freeos/lib/libgdiplus.a
SYSROOT=$T/sysroot/$ARCH/lib

# Таблица: всё, что объектники определяют под именем SystemNative_*.
llvm-nm --defined-only -g "$OUT"/*.o | awk '$2 == "T" && $3 ~ /^SystemNative_/ { print $3 }' | sort -u > "$OUT/exports.txt"
: > "$OUT/gdiplus.txt"
if [ -f "$GDIPLUS" ]; then
  llvm-nm --defined-only -g "$GDIPLUS" | awk '$2 == "T" && $3 ~ /^Gdip/ { print $3 }' | sort -u > "$OUT/gdiplus.txt"
fi
# Функции libc, которые библиотеки классов зовут напрямую, `[DllImport("libc")]`
# (фаза 61b): `uname` — System.Drawing и System.dll отличают так macOS от
# прочих Unix, и без него статический конструктор System.Drawing падает;
# `readlink` — TimeZoneInfo узнаёт так свой пояс. `dlopen("libc")` ищет
# библиотеку `c`.
LIBC_EXPORTS="uname readlink"
# Окна рабочего стола (фаза 62, `<freeos/window.h>` из libfreeos): их зовёт
# драйвер WinForms `XplatUIFreeOS` — `[DllImport("freeos")]`.
FREEOS_EXPORTS="freeos_window_open freeos_window_commit freeos_window_event freeos_window_resize freeos_window_style freeos_window_close freeos_screen"
{
  echo "/* Порождено ports/mono/native.sh — не править руками. */"
  echo "#include <dlfcn.h>"
  for name in $(cat "$OUT/exports.txt" "$OUT/gdiplus.txt") $LIBC_EXPORTS $FREEOS_EXPORTS; do echo "extern void $name(void);"; done
  echo "const struct freeos_export freeos_exports[] = {"
  while read -r name; do
    echo "    {\"System.Native\", \"$name\", (void *)$name},"
    echo "    {\"mono-native\", \"$name\", (void *)$name},"
  done < "$OUT/exports.txt"
  while read -r name; do
    echo "    {\"gdiplus\", \"$name\", (void *)$name},"
  done < "$OUT/gdiplus.txt"
  for name in $LIBC_EXPORTS; do
    echo "    {\"c\", \"$name\", (void *)$name},"
  done
  for name in $FREEOS_EXPORTS; do
    echo "    {\"freeos\", \"$name\", (void *)$name},"
  done
  echo "    {0, 0, 0},"
  echo "};"
} > "$OUT/freeos-exports.c"
$ARCH-freeos-cc -O2 -c "$OUT/freeos-exports.c" -o "$OUT/freeos-exports.o"

# Что дописать компоновщику mono-sgen (`LIBS=$(cat …/libs)`, шаг 4 в
# ports/mono/configure.sh). Пути — с буквой диска: `make` из winget путей
# вида /e/… не понимает.
LIBS="$OUT/freeos-exports.o $OUT/libmono-native.a"
if [ -s "$OUT/gdiplus.txt" ]; then
  LIBS="$LIBS $GDIPLUS"
  for lib in cairo pixman-1 fontconfig expat freetype png16 z; do
    LIBS="$LIBS $SYSROOT/lib$lib.a"
  done
fi
for path in $LIBS; do printf '%s ' "$(cygpath -m "$path")"; done > "$OUT/libs"
echo "System.Native: $(wc -l < "$OUT/exports.txt") functions, gdiplus: $(wc -l < "$OUT/gdiplus.txt"), libc: $(echo $LIBC_EXPORTS | wc -w), freeos: $(echo $FREEOS_EXPORTS | wc -w) -> $OUT"
echo "libs: $(cat "$OUT/libs")"
