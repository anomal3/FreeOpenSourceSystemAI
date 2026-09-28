#!/bin/sh
# Собрать hello.exe и drawing.exe чужим компилятором и снять эталон чужим
# рантаймом (фазы 60, 61b).
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
# Фаза 61b: System.Drawing. Эталон рисует настоящий GDI+ Windows; в stdout —
# только то, что у двух растеризаторов обязано совпасть (см. шапку drawing.cs).
MSYS2_ARG_CONV_EXCL='*' "$CSC" -nologo -optimize+ '-out:initrd\usr\share\mono\drawing.exe' 'ports\mono\samples\drawing.cs'
"$OUT/drawing.exe" | tr -d '\r' > ports/mono/samples/drawing.expected
cat ports/mono/samples/drawing.expected
