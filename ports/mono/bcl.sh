#!/bin/sh
# Библиотеки классов Mono для образа (фазы 60, 61b, 62).
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
# Из monolite берутся mscorlib (ядро библиотеки), System (коллекции, URI,
# процессы) и System.Core (LINQ) — то, без чего не работает консольная
# программа на C#, — и с фазы 61b ещё четыре, замыкание ссылок
# System.Configuration: её типы нужны System.dll уже при загрузке
# `System.Diagnostics.Switch` (поле `switchSettings`), а переключатель заводит у
# себя System.Drawing. Сама она ссылается на System.Xml и System.Security, та —
# на System.Numerics. Без любой из них сборка упала бы не при запуске, а на
# первой ссылке посреди работы.
#
# Остальное собирается здесь из исходников того же архива 6.14.1 компилятором
# C# машины сборки: Roslyn из .NET SDK (`dotnet`), тем же компилятором, которым
# Mono 6 собирает свои библиотеки сама. Ключи — из `Makefile` каждой библиотеки
# и профиля `net_4_x` (`mcs/build/profiles/net_4_x.make`), ссылки — только на
# то, что лежит рядом, поэтому сборки ложатся ровно на наш рантайм. Подпись — та
# же, что у Mono (открытым ключом отложенно, `mono.snk` — полностью): имена
# сборок со строгим именем те же, что у .NET Framework, и программы, собранные
# под Windows, находят их по ним. Проверять подпись Mono не станет.
#
# - System.Drawing (фаза 61b);
# - System.Windows.Forms (фаза 62) — с третьим драйвером окон
#   (`ports/mono/swf/XplatUIFreeOS.cs`), а до неё — всё, на что она ссылается:
#   Accessibility, Mono.Posix, Mono.WebBrowser,
#   System.Runtime.Serialization.Formatters.Soap.
#
# System.Data — исключение, названное вслух. SWF ссылается на неё (привязка
# данных: DataGrid, DataView), и компилировать SWF без неё нельзя, а собрать её
# из исходников Mono — отдельная работа (платформенные списки файлов, ещё две
# сборки под ней). Поэтому SWF компилируется против справочной сборки .NET
# Framework 4.8 (Reference Assemblies, только метаданные), а в образ System.Data
# не кладётся: форма с кнопками её не трогает, а программа с привязкой данных
# получит отказ «нет сборки System.Data» — громко, в том месте, где она нужна.
#
# Использование (из корня репозитория): sh ports/mono/bcl.sh
set -e
ROOT=$(pwd -W)
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
WORK=$(mkdir -p build/toolchain/mono-bcl/work && cd build/toolchain/mono-bcl/work && pwd -W)

# Справочная System.Data (см. шапку) — только для компиляции SWF.
REFDATA="C:/Program Files (x86)/Reference Assemblies/Microsoft/Framework/.NETFramework/v4.8/System.Data.dll"

# Собрать библиотеку: имя, ключ (от каталога библиотеки), ссылки, свои ключи
# компилятора; дальше — файлы сверх её списка. Список файлов — её же
# `<имя>.dll.sources`, пути в нём — от каталога библиотеки; пустые строки
# компилятор принял бы за имя файла, а повторы (mcs, которым Mono собирала себя
# раньше, их прощал) Roslyn отмечает предупреждением — берём по разу.
build_lib () {
  name=$1; key=$2; refs=$3; flags=$4; shift 4
  dir="$SRC/mcs/class/$name"
  rsp="$WORK/$name.rsp"
  grep -v '^[[:space:]]*$' "$dir/$name.dll.sources" | tr -d '\r' | awk '!seen[$0]++' > "$rsp"
  for extra in "$@"; do echo "$extra" >> "$rsp"; done
  refargs=""
  for r in $refs; do
    if [ "$r" = System.Data ]; then
      refargs="$refargs \"-r:$REFDATA\""
    else
      refargs="$refargs \"-r:$ABS/$r.dll\""
    fi
  done
  sign="-keyfile:$key"
  case "$key" in *.pub) sign="$sign -delaysign+" ;; esac
  # `MSYS2_ARG_CONV_EXCL='*'`: ключи csc начинаются с «-» и содержат «:» и
  # «/» — sh из Git переписал бы их как пути.
  (cd "$dir" && export MSYS2_ARG_CONV_EXCL='*' && eval dotnet "\"$CSC\"" -nologo -noconfig -nostdlib -deterministic \
    -langversion:latest -target:library -optimize+ \
    -d:NET_4_0 -d:NET_4_5 -d:NET_4_6 -d:MONO -d:WIN_PLATFORM -nowarn:1699 \
    "\"-r:$ABS/mscorlib.dll\"" $refargs $sign $flags \
    "\"-out:$ABS/$name.dll\"" "\"@$rsp\"") > "$WORK/$name.log" 2>&1 || {
      grep -E "error" "$WORK/$name.log" | head -30; echo "не собралась $name (журнал: $WORK/$name.log)"; exit 1; }
  echo "$name: $(grep -c 'warning' "$WORK/$name.log" || true) предупреждений -> $OUT/$name.dll"
}

build_lib System.Drawing ../msfinal.pub "System" \
  "-unsafe -d:FEATURE_TYPECONVERTER -d:SUPPORTS_WINDOWS_COLORS \
   -resource:Assembly/Mono.ico,Mono.ico -resource:Assembly/Information.ico,Information.ico \
   -resource:Assembly/Error.ico,Error.ico -resource:Assembly/Warning.ico,Warning.ico \
   -resource:Assembly/Question.ico,Question.ico -resource:Assembly/Shield.ico,Shield.ico"

build_lib Accessibility ../msfinal.pub "" ""
build_lib Mono.Posix ../mono.pub "System" "-unsafe -nowarn:618,612"
build_lib Mono.WebBrowser ../mono.snk "System" ""
build_lib System.Runtime.Serialization.Formatters.Soap ../msfinal.pub "System System.Xml" ""

# Ресурсы SWF: курсоры и картинки — файлы дерева, а два `.resources` Mono
# делает из `.resx` своим resgen и, если его нет, берёт готовые `.prebuilt` из
# того же дерева. resgen у машины сборки нет — берём готовые.
SWF="$SRC/mcs/class/System.Windows.Forms"
cp "$SWF/resources/keyboards.resources.prebuilt" "$WORK/keyboards.resources"
cp "$SWF/resources/System.Windows.Forms.resources.prebuilt" "$WORK/System.Windows.Forms.resources"
tr -d '\r' < "$SWF/System.Windows.Forms.dll.resources" \
  | sed "s|^-resource:resources/keyboards.resources|-resource:$WORK/keyboards.resources|; s|^-resource:resources/System.Windows.Forms.resources|-resource:$WORK/System.Windows.Forms.resources|" \
  > "$WORK/swf-resources.rsp"

# `-nowarn:436`: System.Drawing открывает свои внутренние типы SWF (ветка FreeOS
# в `Graphics.FromHwnd`, см. `ports/mono/freeos.patch`), и одноимённые служебные
# типы (SR, Consts, Locale) есть у обеих; свой у SWF побеждает, как и должен, а
# Roslyn отмечает каждое такое место — четыреста с лишним раз.
build_lib System.Windows.Forms ../ecma.pub \
  "System System.Xml System.Drawing Accessibility System.Data Mono.Posix Mono.WebBrowser System.Configuration System.Runtime.Serialization.Formatters.Soap System.Core" \
  "-unsafe -nowarn:618,612,809,436 @$WORK/swf-resources.rsp" \
  "$ROOT/ports/mono/swf/XplatUIFreeOS.cs"

ls -la "$OUT"
