#!/bin/sh
# Сборка Mono 6.14.1 нашим набором (фазы 58b–60). Порт — четыре шага, все из
# корня репозитория, для каждой архитектуры:
#   1. sh ports/mono/configure.sh <арх>   — этот файл;
#   2. make (строка ниже);
#   3. sh ports/mono/native.sh <арх>      — System.Native и таблица экспортов;
#   4. перекомпоновка mono-sgen с ними:
#      make -C build/toolchain/thirdparty/mono-<арх>/mono/mini mono-sgen \
#        LIBS="<native-freeos>/freeos-exports.o <native-freeos>/libmono-native.a"
#      и strip в build/toolchain/sysroot/<арх>/bin/mono.
# Плюс один раз на обе: sh ports/mono/bcl.sh — библиотеки классов. В
# `cargo xtask thirdparty` рецепт ещё не переехал.
#
# Что нужно заранее:
#   cargo xtask sdk
#   исходники: mono-6.14.1.tar.xz с dl.winehq.org (SHA-256
#   3024c97c0bc8cbcd611c401d5f994528704108ceb31f31b28dea4783004d0820),
#   распакованные в build/toolchain/thirdparty/mono-src/ и пропатченные:
#   (cd build/toolchain/thirdparty/mono-src/mono-6.14.1 && patch -p1 < ports/mono/freeos.patch)
#
# Использование (из корня репозитория):
#   sh ports/mono/configure.sh x86_64|aarch64
#   MSYS2_ARG_CONV_EXCL=-DMONO_ make -k -j6 SHELL=C:/PROGRA~1/Git/usr/bin/sh.exe -C build/toolchain/thirdparty/mono-<арх>/mono
# `SHELL` — путь к sh без пробелов: make из winget подставляет его в рецепты
# без кавычек, и «C:/Program Files/...» ломает первую же команду.
# `MSYS2_ARG_CONV_EXCL=-DMONO_` — по той же причине, что ниже у configure, но
# на шаге сборки: sh из Git Bash, запуская компилятор (программу Windows),
# переписывает `-DMONO_ASSEMBLIES="/usr/lib"` в «C:/Program Files/Git/usr/lib».
set -e
ARCH=${1:-x86_64}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
T=$ROOT/build/toolchain
export PATH="$T/bin:/c/Program Files/LLVM/bin:$PATH"
W=$T/thirdparty/mono-$ARCH
rm -rf "$W"; mkdir -p "$W"; cd "$W"
# `--host=<арх>-freeos`: `config.sub` из патча знает систему, и configure
# находит x86_64-freeos-cc, -ar, -ranlib… сам, по приставке. C++ компилятора с
# именем g++ у нас нет — его задаём; `ld` libtool ищет, спрашивая компилятор
# `-print-prog-name=ld`, — его тоже.
#
# `ac_cv_have_dev_random=yes` — ответ кеша для кросс-сборки: configure ищет
# /dev/random на машине сборки (здесь это Windows), а у цели источник энтропии
# есть — `getentropy` поверх `SYS_RANDOM`, её Mono и берёт первой.
#
# `--prefix=/usr --sysconfdir=/etc` (фаза 60): библиотеки классов Mono ищет
# по пути, вшитому при сборке, — `<prefix>/lib/mono/4.5/mscorlib.dll`, — потому
# что узнать, откуда запущена, ей не у кого (`/proc/self/exe` у нас нет). Путь
# тот же, что у пакетов Mono для Linux: `/usr/lib/mono`, настройки — `/etc/mono`.
# `MSYS2_ARG_CONV_EXCL` обязателен: Git Bash молча переписывает аргумент
# `--prefix=/usr` в `--prefix=C:/Program Files/Git/usr` — путь его же
# установки, — и Mono во FreeOS искала бы mscorlib там.
export MSYS2_ARG_CONV_EXCL='--prefix=;--sysconfdir='
#
# `--enable-minimal`: без сокетов (libc отвечает на них ENOSYS) и без
# счётчиков, отладчика и присоединения. `processes` не выключается: в 6.14 без
# них не собирается рантайм (функции `mono_w32process_*` исчезают целиком).
../mono-src/mono-6.14.1/configure --host=$ARCH-freeos --prefix=/usr --sysconfdir=/etc \
  CXX=$ARCH-freeos-cc CXXCPP="$ARCH-freeos-cc -E" LD=$ARCH-freeos-ld OBJDUMP=$ARCH-freeos-objdump \
  ac_cv_have_dev_random=yes \
  --disable-boehm --with-sgen=yes --enable-cooperative-suspend --with-sigaltstack=no \
  --with-tls=__thread --disable-mcs-build --disable-btls --disable-nls --without-ikvm-native \
  --with-static_mono=yes --with-shared_mono=no --disable-dtrace --disable-support-build \
  --with-libgdiplus=no \
  --enable-minimal=sockets,perfcounters,shared_perfcounters,attach,lldb,mdb
