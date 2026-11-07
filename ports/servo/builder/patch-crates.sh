#!/bin/sh
# Правки FreeOS в C-части крейтов Servo, которые не знают нашу ОС (N4).
# Подключать после freeos-env.sh: `. patch-crates.sh` — готовит копии крейтов
# в /cache/src и дописывает их в /tmp/patches.toml ([patch.crates-io]).
#
# aws-lc-sys: случайность. На «прочих» ОС aws-lc читает /dev/urandom, а у
# родных программ FreeOS /dev нет (он есть только в слое Linux); есть
# getentropy() поверх SYS_RANDOM — как у FreeBSD/OpenBSD/macOS. Одна строка в
# crypto/rand_extra/internal.h (как у mozjs: mfbt/RandomNum.cpp).
AWS_LC_SYS=aws-lc-sys-0.47.0
src=$(ls -d /opt/cargo/registry/src/*/$AWS_LC_SYS 2>/dev/null | head -1)
if [ -z "$src" ]; then
    # Крейт ещё не скачан — скачать через cargo в пустом проекте нельзя без
    # сети к индексу; проще дать cargo скачать его обычной сборкой.
    echo "patch-crates: $AWS_LC_SYS нет в реестре — сначала обычный cargo fetch"
    (cd /work/ports/servo/depsprobe && cargo fetch -q) || true
    src=$(ls -d /opt/cargo/registry/src/*/$AWS_LC_SYS | head -1)
fi
dst=/cache/src/$AWS_LC_SYS
h=$dst/aws-lc/crypto/rand_extra/internal.h
if ! grep -q __freeos__ "$h" 2>/dev/null; then
    rm -rf "$dst"
    mkdir -p /cache/src
    cp -r "$src" "$dst"
    sed -i 's/^#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || \\$/#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || defined(__freeos__) || \\/' "$h"
    grep -q __freeos__ "$h" || { echo "patch-crates: правка $h не легла"; exit 1; }
fi
cat >> /tmp/patches.toml <<EOF
aws-lc-sys = { path = "$dst" }
EOF
echo "patch-crates: $AWS_LC_SYS (getentropy) -> $dst"
