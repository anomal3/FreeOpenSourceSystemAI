#!/bin/sh
# Собрать hello.exe чужим компилятором и снять эталон чужим рантаймом (фаза 60).
#
# Компилятор — csc из .NET Framework 4 (есть на любой Windows), рантайм для
# эталона — сам .NET Framework: тот же файл, запущенный здесь, обязан
# напечатать ровно то, что Mono во FreeOS (сценарий `mono` сверяет строки).
#
# Использование (из корня репозитория, Git Bash на Windows):
#   sh ports/mono/samples/build.sh
set -e
CSC=/c/Windows/Microsoft.NET/Framework64/v4.0.30319/csc.exe
OUT=initrd/usr/share/mono
mkdir -p "$OUT"
# Пути — через обратную косую: csc принимает «/» за начало ключа.
MSYS2_ARG_CONV_EXCL='*' "$CSC" -nologo -optimize+ '-out:initrd\usr\share\mono\hello.exe' 'ports\mono\samples\hello.cs'
"$OUT/hello.exe" a b | tr -d '\r' > ports/mono/samples/hello.expected
cat ports/mono/samples/hello.expected
