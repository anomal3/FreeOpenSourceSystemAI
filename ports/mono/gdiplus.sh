#!/bin/sh
# libgdiplus для Mono во FreeOS (фаза 61b): на ней рисует `System.Drawing`.
#
# libgdiplus — библиотека проекта Mono (теперь её ведёт Wine), и собирается она
# **поверх сборки Mono своей архитектуры**, а не поверх набора: вместо glib ей
# отдаётся eglib самой Mono (см. ports/mono/glib/glib.h). Поэтому это шаг порта
# Mono, а не `cargo xtask thirdparty`. Всё, что ниже неё, — cairo, pixman,
# freetype, fontconfig, expat, libpng, zlib — thirdparty уже положил в sysroot.
#
# Шаги:
#   1. glib = eglib: копия заголовков eglib этой архитектуры, наша прослойка
#      `glib.h` поверх и `glib-2.0.pc` — не в sysroot, а рядом со сборкой Mono;
#      configure видит его через PKG_CONFIG_PATH (наш pkg-config его читает).
#      В sysroot он соврал бы следующему порту, что настоящая glib в наборе есть;
#   2. libgdiplus своим configure и make, статически, в mono-<арх>/gdiplus-freeos.
#      Без X11 (окон X у нас нет) и без JPEG, TIFF, GIF и EXIF: их библиотек в
#      наборе нет. BMP, PNG, ICO, EMF и WMF libgdiplus читает и пишет сама
#      (PNG — через libpng).
# Дальше — как у System.Native: `ports/mono/native.sh <арх>` находит
# libgdiplus.a, вносит её функции `Gdip*` в таблицу экспортов под именем
# `gdiplus` и пишет список библиотек для перекомпоновки mono.
#
# Исходники: libgdiplus-6.2.tar.gz с dl.winehq.org (архив с готовым configure;
# у архива с GitHub его нет, а autotools на машине сборки нет). SHA-256 — тот,
# что опубликован в формуле Homebrew (сверено 2026-09-28).
#
# Использование (из корня репозитория, после configure и make Mono этой
# архитектуры и `cargo xtask thirdparty`):
#   sh ports/mono/gdiplus.sh x86_64|aarch64
set -e
ARCH=${1:-x86_64}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
T=$ROOT/build/toolchain
TP=$T/thirdparty
export PATH="$T/bin:/c/Program Files/LLVM/bin:$PATH"
MONO_SRC=$TP/mono-src/mono-6.14.1
B=$TP/mono-$ARCH
[ -f "$B/mono/eglib/eglib-config.h" ] || { echo "нет сборки Mono для $ARCH ($B): сначала ports/mono/configure.sh и make"; exit 1; }
[ -f "$T/sysroot/$ARCH/lib/libcairo.a" ] || { echo "нет cairo в наборе $ARCH: cargo xtask thirdparty"; exit 1; }

URL=https://dl.winehq.org/mono/sources/libgdiplus/libgdiplus-6.2.tar.gz
SHA=683adb7d99d03f6ee7985173a206a2243f76632682334ced4cae2fcd20c83bc9
ARCHIVE=$TP/dl/libgdiplus-6.2.tar.gz
if [ ! -f "$ARCHIVE" ]; then
  mkdir -p "$TP/dl"
  curl --location --fail --silent --show-error --output "$ARCHIVE" "$URL"
fi
GOT=$(sha256sum "$ARCHIVE" | cut -d' ' -f1)
[ "$GOT" = "$SHA" ] || { echo "хеш $ARCHIVE не тот: $GOT, ожидался $SHA"; exit 1; }
# Распакованное дерево — эталон, его не трогаем; собирается копия.
[ -d "$TP/libgdiplus-6.2" ] || (cd "$TP" && tar -xzf dl/libgdiplus-6.2.tar.gz)

# 1. glib = eglib.
GLIB=$B/glib-freeos
rm -rf "$GLIB"; mkdir -p "$GLIB/include/eglib" "$GLIB/pkgconfig"
cp "$ROOT/ports/mono/glib/glib.h" "$GLIB/include/glib.h"
cp "$MONO_SRC/mono/eglib/glib.h" "$MONO_SRC/mono/eglib/eglib-remap.h" "$GLIB/include/eglib/"
cp "$B/mono/eglib/eglib-config.h" "$GLIB/include/eglib/"
# Версия — та, которой eglib себя называет (`_EGLIB_MAJOR/MIDDLE/MINOR`);
# libgdiplus просит «не старее 2.2.3». Библиотек в `Libs` нет намеренно:
# единственный, кто компонует libgdiplus, — сама mono, а eglib в ней уже есть.
# Названная здесь, она ещё и влилась бы в libgdiplus.a: libeglib.la у Mono —
# вспомогательная библиотека libtool, и такие он вкладывает в каждую
# собираемую с ними статическую.
cat > "$GLIB/pkgconfig/glib-2.0.pc" <<EOF
prefix=$(cygpath -m "$GLIB")
includedir=\${prefix}/include

Name: GLib (eglib of Mono 6.14.1)
Description: glib for libgdiplus on FreeOS is the eglib Mono already carries
Version: 2.4.0
Libs:
Cflags: -I\${includedir} -I\${includedir}/eglib
EOF

# 2. libgdiplus.
W=$TP/gdiplus-$ARCH
OUT=$B/gdiplus-freeos
rm -rf "$W" "$OUT"
cp -r "$TP/libgdiplus-6.2" "$W"
cp "$ROOT/ports/autotools/config.sub" "$W/config.sub"
cd "$W"
# `PKG_CONFIG` — явно и полным путём: configure libgdiplus ищет просто
# `pkg-config` (`AC_PATH_PROG`), а не `<хост>-pkg-config`, как
# `PKG_PROG_PKG_CONFIG` у остальных, и на машине сборки его нет вовсе; готовое
# значение `AC_PATH_PROG` принимает, только если оно — абсолютный путь.
# `LD`, `CXX`, `CXXCPP` — как у всех проектов под libtool (фаза 61a).
PKG_CONFIG_PATH=$(cygpath -m "$GLIB/pkgconfig") ./configure --host=$ARCH-freeos --prefix="$(cygpath -m "$OUT")" \
  --disable-shared --enable-static \
  LD=$ARCH-freeos-ld CXX=$ARCH-freeos-cc CXXCPP="$ARCH-freeos-cc -E" \
  PKG_CONFIG="$(cygpath -m "$T/bin/$ARCH-freeos-pkg-config.exe")" \
  --without-x11 --without-libjpeg --without-libtiff --without-libgif --without-libexif \
  > "$TP/gdiplus-$ARCH-configure.log" 2>&1 || { tail -30 "$TP/gdiplus-$ARCH-configure.log"; exit 1; }
# Только библиотека: тесты — программы на C и C++ (googletest), им нужно то,
# чего в системе нет.
MK="make -j6 SHELL=C:/PROGRA~1/Git/usr/bin/sh.exe SUBDIRS=src"
$MK > "$TP/gdiplus-$ARCH-make.log" 2>&1 || { grep -E "error" "$TP/gdiplus-$ARCH-make.log" | head -30; exit 1; }
$MK install >> "$TP/gdiplus-$ARCH-make.log" 2>&1 || { tail -20 "$TP/gdiplus-$ARCH-make.log"; exit 1; }
ls -la "$OUT/lib/libgdiplus.a"
echo "libgdiplus 6.2 для $ARCH -> $OUT (дальше: sh ports/mono/native.sh $ARCH)"
