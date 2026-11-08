#!/bin/sh
# Правки FreeOS в C-части крейтов Servo, которые не знают нашу ОС (N4).
# Подключать после freeos-env.sh: `. patch-crates.sh` — готовит копии крейтов
# в /cache/src и дописывает их в /tmp/patches.toml ([patch.crates-io]).
# Копия пересоздаётся, только если в ней ещё нет метки правки (`__freeos__`).

# copy_crate <имя-версия> <файл с меткой> — копия крейта из реестра в
# /cache/src; печатает путь. 0 — копия свежая (править), 1 — уже поправлена.
copy_crate() {
    dst=/cache/src/$1
    if grep -q __freeos__ "$dst/$2" 2>/dev/null; then
        return 1
    fi
    src=$(ls -d /opt/cargo/registry/src/*/$1 2>/dev/null | head -1)
    if [ -z "$src" ]; then
        # Крейт ещё не скачан — пусть cargo скачает зависимости пробы.
        (cd /work/ports/servo/depsprobe && cargo fetch -q) || true
        src=$(ls -d /opt/cargo/registry/src/*/$1 | head -1)
    fi
    rm -rf "$dst"
    mkdir -p /cache/src
    cp -r "$src" "$dst"
    return 0
}

# aws-lc-sys: случайность. На «прочих» ОС aws-lc читает /dev/urandom, а у
# родных программ FreeOS /dev нет (он есть только в слое Linux); есть
# getentropy() поверх SYS_RANDOM — как у FreeBSD/OpenBSD/macOS. Одна строка в
# crypto/rand_extra/internal.h (как у mozjs: mfbt/RandomNum.cpp).
AWS_LC_SYS=aws-lc-sys-0.47.0
h=aws-lc/crypto/rand_extra/internal.h
if copy_crate $AWS_LC_SYS $h; then
    sed -i 's/^#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || \\$/#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || defined(__freeos__) || \\/' "/cache/src/$AWS_LC_SYS/$h"
    grep -q __freeos__ "/cache/src/$AWS_LC_SYS/$h" || { echo "patch-crates: правка $h не легла"; exit 1; }
fi

cat >> /tmp/patches.toml <<EOF
aws-lc-sys = { path = "/cache/src/$AWS_LC_SYS" }
EOF
echo "patch-crates: $AWS_LC_SYS (getentropy) -> /cache/src"
