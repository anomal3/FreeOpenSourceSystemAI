#!/bin/sh
# Библиотеки классов Mono для образа (фаза 60).
#
# Сборки IL не зависят от процессора, поэтому они одни на обе архитектуры. А
# собирать их нечем: библиотеки классов Mono собираются только работающей Mono
# на машине сборки. Но в архиве исходников 6.14.1 они уже лежат готовыми —
# `mcs/class/lib/monolite-linux/<GUID>/` — это «monolite», начальный набор, на
# котором Mono собирает сама себя. Его GUID — `MONO_CORLIB_VERSION` из
# `configure.ac`, и рантайм сверяет его с тем, что вшит в mscorlib: чужой
# mscorlib (например, из пакета 6.12) он отверг бы при запуске. Эти — ровно
# под наш рантайм.
#
# Берутся три сборки — то, без чего не работает консольная программа на C#:
# mscorlib (ядро библиотеки), System (коллекции, URI, процессы) и System.Core
# (LINQ). Остальное из monolite — компилятор и то, что нужно ему, — позже.
#
# Использование (из корня репозитория): sh ports/mono/bcl.sh
set -e
SRC=build/toolchain/thirdparty/mono-src/mono-6.14.1
GUID=$(sed -n 's/^MONO_CORLIB_VERSION=//p' "$SRC/configure.ac")
LITE="$SRC/mcs/class/lib/monolite-linux/$GUID"
OUT=build/toolchain/mono-bcl/4.5
[ -f "$LITE/mscorlib.dll" ] || { echo "нет $LITE/mscorlib.dll"; exit 1; }
mkdir -p "$OUT"
for name in mscorlib.dll System.dll System.Core.dll; do
  cp "$LITE/$name" "$OUT/$name"
done
echo "corlib $GUID -> $OUT"
ls -la "$OUT"
