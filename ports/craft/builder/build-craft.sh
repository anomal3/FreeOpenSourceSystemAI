#!/bin/sh
# Приложение Craft (GridCraft, WordCraft, PdfCraft, MailCraft) под
# <арх>-unknown-freeos — НАТИВНО, без слоя Linux. В контейнере fossai-builder:
#
#   docker run --rm -v <проект>:/work -v fossai-craft:/cache \
#       -v fossai-cargo-registry:/opt/cargo/registry \
#       fossai-builder sh /work/ports/craft/builder/build-craft.sh gridcraft [x86_64] [check]
#
# Берёт исходник выпуска (тег из ports/craft/versions.toml или $CRAFT_TAG) с
# GitHub — без форка: клон в /cache/craft/<app>-<тег>, правки FreeOS
# ports/craft/<app>/*.patch (`git apply`), подмены крейтов через --config:
#   eframe  → ports/craft/host/eframe  (окно FreeOS, egui рисует процессор)
#   rfd     → ports/craft/host/rfd     (выбор файла — своё окно на egui)
#   arboard → ports/craft/host/arboard (буфер обмена SYS_CLIPBOARD)
#   winit   → ports/craft/host/winit   (пустая заглушка)
# Готовая программа — build/linux/<app>-<арх>, рядом <app>-<арх>.version.
set -e
APP=${1:?app}
ARCH=${2:-x86_64}
MODE=${3:-build}
# dash не передаёт аргументы в `.` — freeos-env.sh читает $1 этого скрипта.
set -- "$ARCH"
. /work/ports/servo/builder/freeos-env.sh "$ARCH"
# Общий каталог сборки для всех четырёх: зависимости (egui, image, ring…)
# собираются один раз.
export CARGO_TARGET_DIR=/cache/target-craft

# Поля приложения из versions.toml: field <имя>.
field() {
    awk -v app="[$APP]" -v key="$1" '
        /^\[/ { inside = ($0 == app) }
        inside && $1 == key { sub(/^[^=]*= *"/, ""); sub(/".*$/, ""); print; exit }
    ' /work/ports/craft/versions.toml
}
REPO=$(field repo)
TAG=${CRAFT_TAG:-$(field tag)}
BIN=$(field bin)
PKG=$(field cargo_package)
[ -n "$REPO" ] && [ -n "$TAG" ] || { echo "build-craft: нет $APP в versions.toml"; exit 1; }
echo "build-craft: $APP $TAG ($REPO), $ARCH"

# Исходник выпуска: неглубокий клон тега; повторная сборка — сброс правок.
SRC=/cache/craft/$APP-$TAG
if [ ! -d "$SRC/.git" ]; then
    rm -rf "$SRC"
    mkdir -p /cache/craft
    git clone -q --depth 1 --branch "$TAG" "https://github.com/$REPO.git" "$SRC"
fi
cd "$SRC"
git -c safe.directory='*' reset -q --hard
git -c safe.directory='*' clean -qfdx -e target
for p in /work/ports/craft/common/*.patch /work/ports/craft/$APP/*.patch; do
    [ -f "$p" ] || continue
    echo "build-craft: patch $(basename "$p")"
    git -c safe.directory='*' apply --whitespace=nowarn "$p"
done
# Правки, которые проще скриптом (Cargo.toml всех членов и т. п.).
[ -f /work/ports/craft/$APP/patch.sh ] && . /work/ports/craft/$APP/patch.sh

# Крейты экосистемы с веткой FreeOS, которых нет в общих подменах xtask
# (patch-craft-crates.sh: getrandom 0.3 и др.).
. /work/ports/craft/builder/patch-craft-crates.sh
# C/C++ крейтов (ring, zstd, libz…) — как у Servo: libc++, -D_DEFAULT_SOURCE.
eval "export CXXSTDLIB_$KEY=c++"
export RUSTFLAGS="$RUSTFLAGS -C link-arg=-lc++ -C link-arg=-lc++abi -C link-arg=-lunwind"
CFLAGS_T=-D_DEFAULT_SOURCE
[ "$ARCH" = aarch64 ] && CFLAGS_T="$CFLAGS_T -DOPENSSL_STATIC_ARMCAP"
eval "export CFLAGS_$KEY=\"$CFLAGS_T\""
export LIBZ_SYS_STATIC=1

# libc FreeOS (build/rust-std/src/libc, 0.2.189) — копия с версией 0.2.999:
# Cargo.lock приложений держит libc новее (0.2.190), и подмена с меньшей
# версией молча не применялась бы (`^0.2.190` её не принимает). Копия
# обновляется, только когда исходник новее — иначе cargo пересобирал бы
# всё, что стоит на libc.
LIBC_SRC=/work/build/rust-std/src/libc
LIBC_DST=/cache/src/libc-craft-999
if [ ! -f $LIBC_DST/.stamp ] || [ -n "$(find $LIBC_SRC -type f -newer $LIBC_DST/.stamp | head -1)" ]; then
    rm -rf $LIBC_DST
    mkdir -p /cache/src
    cp -r $LIBC_SRC $LIBC_DST
    sed -i '0,/^version = /s/^version = .*/version = "0.2.999"/' $LIBC_DST/Cargo.toml
    touch $LIBC_DST/.stamp
fi
# Вторым ключом: прежняя строка `libc` (0.2.189) нужна самому std (-Zbuild-std).
echo "libc-0-2-999 = { path = \"$LIBC_DST\", package = \"libc\" }" >> /tmp/patches.toml

H=/work/ports/craft/host
cat >> /tmp/patches.toml <<EOF
eframe = { path = "$H/eframe" }
rfd = { path = "$H/rfd" }
arboard = { path = "$H/arboard" }
winit = { path = "$H/winit" }
EOF
# Свои git-зависимости приложений (форк egui storytold) — та же подмена и
# для их источника: config-[patch] на crates-io не достаёт до
# [patch."https://github.com/storytold/egui.git"], поэтому eframe из форка
# заменяется в самом Cargo.toml (patch.sh приложения / общий шаг ниже).
sed -i "s#^eframe = { git = \"https://github.com/storytold/egui.git\"[^}]*}#eframe = { path = \"$H/eframe\" }#" Cargo.toml
sed -i "s#^winit = { git = \"https://github.com/storytold/winit.git\"[^}]*}#winit = { path = \"$H/winit\" }#" Cargo.toml

# Подмена с версией, отличной от записанной в Cargo.lock (libc 0.2.190,
# rustix 1.1.5, tokio 1.53.2…), cargo молча не применяет («patch was not
# used»): запись переводится на версию подмены `cargo update --precise`
# (без --precise взялась бы новейшая с crates.io, а не наша).
semver_key() { echo "$1" | awk -F. '{ print ($1 == "0") ? "0." $2 : $1 }'; }
# Готовый Cargo.lock для этого набора подмен — из кеша (перебор `cargo
# update` идёт минуты).
LOCKKEY=$(cat /tmp/patches.toml Cargo.lock Cargo.toml | sha256sum | cut -c1-16)
LOCKED=/cache/locks/$APP-$LOCKKEY.lock
if [ -f "$LOCKED" ]; then
    cp -f "$LOCKED" Cargo.lock
    echo "build-craft: Cargo.lock из кеша ($LOCKKEY)"
else
grep -o '^[a-z0-9_-]* = { path = "[^"]*"\( *, *package = "[^"]*"\)\?' /tmp/patches.toml | while read -r line; do
    dir=$(echo "$line" | sed 's/.*path = "\([^"]*\)".*/\1/')
    name=$(echo "$line" | sed -n 's/.*package = "\([^"]*\)".*/\1/p')
    [ -n "$name" ] || name=${line%% *}
    want=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -1)
    for v in $(awk -v n="$name" '$0 == "name = \"" n "\"" { getline; gsub(/version = |"/, ""); print }' Cargo.lock); do
        [ "$v" = "$want" ] && continue
        # Только в пределах совместимых по semver (0.2.x ↔ 0.2.y, 1.x ↔ 1.y).
        [ "$(semver_key "$v")" = "$(semver_key "$want")" ] || continue
        cargo update -q --config /tmp/patches.toml -p "$name@$v" --precise "$want" 2>/dev/null \
            && echo "build-craft: $name $v -> $want (подмена FreeOS)" || true
    done
done
mkdir -p /cache/locks
cp -f Cargo.lock "$LOCKED"
fi

if [ "$MODE" = check ]; then
    cargo check --keep-going --release -Zbuild-std=std,panic_abort -Zjson-target-spec \
        --target "$FREEOS_TARGET_SPEC" --config /tmp/patches.toml -p "$PKG" --bin "$BIN" 2>&1 | tee /cache/craft-check-$APP-$ARCH.txt
    exit 0
fi
# Код выхода cargo — мимо tee (у dash нет pipefail).
{ freeos_build "$SRC" -p "$PKG" --bin "$BIN"; echo $? > /tmp/craft-rc; } 2>&1 | tee /cache/craft-build-$APP-$ARCH.txt
[ "$(cat /tmp/craft-rc)" = 0 ] || { echo "build-craft: $APP не собрался"; exit 1; }
OUT="$CARGO_TARGET_DIR/$TRIPLE/release/$BIN"
mkdir -p /work/build/linux
cp -f "$OUT" "/work/build/linux/$APP-$ARCH"
echo "$TAG" > "/work/build/linux/$APP-$ARCH.version"
# Для пакета (xtask craft): значок и лицензии/уведомления выпуска.
SHARE=/work/build/linux/craft-$APP
mkdir -p "$SHARE"
for f in LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
    [ -f "$SRC/$f" ] && cp -f "$SRC/$f" "$SHARE/$f"
done
ICON=$(field icon)
[ -n "$ICON" ] && [ -f "$SRC/$ICON" ] && cp -f "$SRC/$ICON" "$SHARE/icon.png"
ls -la "/work/build/linux/$APP-$ARCH"
