#!/bin/sh
# Исходники mozjs_sys (SpiderMonkey для Servo) с ОС freeos:
# build/servo/mozjs_sys-<версия> = крейт с crates.io (хеш сверяется) + freeos.patch.
# Каталог — git с исходным состоянием в первом коммите: новые правки снимаются
#   cd build/servo/mozjs_sys-<версия> && git diff > ../../../ports/servo/mozjs/freeos.patch
#
# Запуск — в контейнере fossai-builder (его зовёт build-jsprobe.sh) или в любом
# sh с curl, tar, git, sha256sum.
set -e
VERSION=153.6.0-1
# SHA-256 mozjs_sys-153.6.0-1.crate (static.crates.io, 07.11.2026).
SHA256=199dc451e15dc64f4c879abccb1c9956504dfebdc9f2ab0ecded313d481c949a
W=${W:-/work}
DIR=$W/build/servo
SRC=$DIR/mozjs_sys-$VERSION

[ -d "$SRC" ] && { echo "mozjs: $SRC уже есть"; exit 0; }
mkdir -p "$DIR"
CRATE=$DIR/mozjs_sys-$VERSION.crate
[ -f "$CRATE" ] || curl -sSfL -o "$CRATE" "https://static.crates.io/crates/mozjs_sys/mozjs_sys-$VERSION.crate"
echo "$SHA256  $CRATE" | sha256sum -c -
tar -xzf "$CRATE" -C "$DIR"
cd "$SRC"
git init -q
git add -A
git -c user.name=fossai -c user.email=fossai@localhost commit -qm "mozjs_sys $VERSION (crates.io)"
git apply --whitespace=nowarn "$W/ports/servo/mozjs/freeos.patch"
echo "mozjs: $SRC готов (freeos.patch наложен)"
