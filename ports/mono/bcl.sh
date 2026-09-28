#!/bin/sh
# Библиотеки классов Mono для образа (фазы 60, 61b).
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
# (LINQ). И с фазы 61b — ещё четыре, замыкание ссылок System.Configuration:
# её типы нужны System.dll уже при загрузке `System.Diagnostics.Switch` (поле
# `switchSettings`), а переключатель заводит у себя System.Drawing. Сама она
# ссылается на System.Xml и System.Security, та — на System.Numerics. Без
# любой из них сборка упала бы не при запуске, а на первой ссылке посреди
# работы. Остальное из monolite — компилятор и то, что нужно ему, — позже.
#
# System.Drawing (фаза 61b) в monolite нет — компилятору она не нужна. Она
# собирается здесь из исходников того же архива 6.14.1 компилятором C# машины
# сборки: Roslyn из .NET SDK (`dotnet`), тот же компилятор, которым Mono 6
# собирает свои библиотеки сама. Ключи — её `Makefile` и профиль `net_4_x`
# (`mcs/build/profiles/net_4_x.make`), ссылки — только mscorlib и System из
# monolite, поэтому сборка ложится ровно на наш рантайм. Подпись — отложенная,
# открытым ключом `msfinal.pub`, как у Mono: имя сборки со строгим именем то
# же, что у .NET Framework (`b03f5f7f11d50a3a`), и программы, собранные под
# Windows, находят её по нему. Проверять подпись Mono не станет.
#
# Использование (из корня репозитория): sh ports/mono/bcl.sh
set -e
SRC=build/toolchain/thirdparty/mono-src/mono-6.14.1
GUID=$(sed -n 's/^MONO_CORLIB_VERSION=//p' "$SRC/configure.ac")
LITE="$SRC/mcs/class/lib/monolite-linux/$GUID"
OUT=build/toolchain/mono-bcl/4.5
[ -f "$LITE/mscorlib.dll" ] || { echo "нет $LITE/mscorlib.dll"; exit 1; }
mkdir -p "$OUT"
for name in mscorlib.dll System.dll System.Core.dll \
  System.Configuration.dll System.Xml.dll System.Security.dll System.Numerics.dll; do
  cp "$LITE/$name" "$OUT/$name"
done
echo "corlib $GUID -> $OUT"

# Roslyn — из самого нового установленного .NET SDK.
SDK=$(dotnet --list-sdks | tail -1 | sed 's/^\([^ ]*\) \[\(.*\)\]$/\2\/\1/' | tr '\\' '/')
CSC="$SDK/Roslyn/bincore/csc.dll"
[ -f "$CSC" ] || { echo "нет компилятора C#: $CSC (нужен .NET SDK: winget install Microsoft.DotNet.SDK.9)"; exit 1; }
ABS=$(cd "$OUT" && pwd -W)
LITE_ABS=$(cd "$LITE" && pwd -W)
DRAWING="$SRC/mcs/class/System.Drawing"
# Список файлов — их же `System.Drawing.dll.sources`, пути в нём — от каталога
# библиотеки; пустые строки компилятор принял бы за имя файла. Два файла в
# списке повторяются: mcs, которым Mono собирала себя раньше, повторы прощал,
# Roslyn о каждом предупреждает — берём по разу.
grep -v '^[[:space:]]*$' "$DRAWING/System.Drawing.dll.sources" | tr -d '\r' | awk '!seen[$0]++' > "$ABS/System.Drawing.rsp"
# `MSYS2_ARG_CONV_EXCL='*'`: ключи csc начинаются с «-» и содержат «:» и «/» —
# sh из Git переписал бы их как пути.
(cd "$DRAWING" && MSYS2_ARG_CONV_EXCL='*' dotnet "$CSC" -nologo -noconfig -nostdlib -deterministic \
  -langversion:latest -target:library -unsafe -optimize+ \
  -d:NET_4_0 -d:NET_4_5 -d:NET_4_6 -d:MONO -d:WIN_PLATFORM -d:FEATURE_TYPECONVERTER -d:SUPPORTS_WINDOWS_COLORS \
  -nowarn:1699 \
  "-r:$LITE_ABS/mscorlib.dll" "-r:$LITE_ABS/System.dll" \
  -keyfile:../msfinal.pub -delaysign+ \
  -resource:Assembly/Mono.ico,Mono.ico -resource:Assembly/Information.ico,Information.ico \
  -resource:Assembly/Error.ico,Error.ico -resource:Assembly/Warning.ico,Warning.ico \
  -resource:Assembly/Question.ico,Question.ico -resource:Assembly/Shield.ico,Shield.ico \
  "-out:$ABS/System.Drawing.dll" "@$ABS/System.Drawing.rsp")
rm -f "$ABS/System.Drawing.rsp"
ls -la "$OUT"
