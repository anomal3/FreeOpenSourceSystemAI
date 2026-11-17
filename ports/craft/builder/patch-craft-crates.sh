#!/bin/sh
# Крейты экосистемы, которым у приложений Craft нужна ветка FreeOS, а в общих
# подменах xtask (build/rust-std/src/patches.toml) их версии нет. Подключать
# из build-craft.sh (cwd — исходник приложения с Cargo.lock): копии — в
# /cache/src, строки [patch.crates-io] — в /tmp/patches.toml.
#
# Правка та же, что у xtask (`LIKE_REDOX`): ОС-образец → `freeos` рядом.

# like_os <имя> <версия> <образец>: скачать крейт с crates.io (хеш из Cargo.lock),
# поставить `target_os = "freeos"` рядом с каждым `target_os = "<образец>"`
# (все такие места у этих крейтов — внутри any(...)).
like_os() {
    name=$1; version=$2; like=$3
    dst=/cache/src/$name-$version
    if ! grep -rqs 'target_os = "freeos"' "$dst/src"; then
        sum=$(awk -v n="$name" -v v="$version" '
            $0 == "name = \"" n "\"" { hit = 1; next }
            hit && $0 == "version = \"" v "\"" { ok = 1; next }
            hit && ok && /^checksum/ { gsub(/.*= "|"/, ""); print; exit }
            /^\[\[package\]\]/ { hit = 0; ok = 0 }' Cargo.lock)
        rm -rf "$dst"
        mkdir -p /cache/src
        curl -sSfL "https://static.crates.io/crates/$name/$name-$version.crate" -o /tmp/$name.crate
        if [ -n "$sum" ]; then
            echo "$sum  /tmp/$name.crate" | sha256sum -c - >/dev/null || { echo "patch-craft-crates: хеш $name-$version не сошёлся"; exit 1; }
        fi
        tar -xzf /tmp/$name.crate -C /cache/src
        find "$dst" \( -name '*.rs' -o -name Cargo.toml \) -exec \
            sed -i "s/target_os = \"$like\"/target_os = \"$like\", target_os = \"freeos\"/g" {} +
        grep -rqs 'target_os = "freeos"' "$dst/src" || { echo "patch-craft-crates: правка $name не легла"; exit 1; }
    fi
    key=$name-$(echo "$version" | tr . -)
    echo "$key = { path = \"$dst\", package = \"$name\" }" >> /tmp/patches.toml
    echo "patch-craft-crates: $name $version (как $like)"
}

# aws-lc-sys (PdfCraft: подписи PDF через aws-lc-rs) — та же правка, что у
# Servo (ports/servo/builder/patch-crates.sh): случайность через getentropy(),
# а не /dev/urandom (у родных программ FreeOS /dev нет).
for v in $(awk '$0 == "name = \"aws-lc-sys\"" { getline; gsub(/version = |"/, ""); print }' Cargo.lock); do
    dst=/cache/src/aws-lc-sys-$v
    h=aws-lc/crypto/rand_extra/internal.h
    if ! grep -q __freeos__ "$dst/$h" 2>/dev/null; then
        rm -rf "$dst"
        mkdir -p /cache/src
        curl -sSfL "https://static.crates.io/crates/aws-lc-sys/aws-lc-sys-$v.crate" | tar -xz -C /cache/src
        sed -i 's/^#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || \\$/#elif defined(OPENSSL_MACOS) || defined(OPENSSL_OPENBSD) || defined(__freeos__) || \\/' "$dst/$h"
        grep -q __freeos__ "$dst/$h" || { echo "patch-craft-crates: правка aws-lc-sys $v не легла"; exit 1; }
    fi
    echo "aws-lc-sys-$(echo $v | tr . -) = { path = \"$dst\", package = \"aws-lc-sys\" }" >> /tmp/patches.toml
    echo "patch-craft-crates: aws-lc-sys $v (getentropy)"
    export AWS_LC_SYS_NO_JITTER_ENTROPY=1
done

# crate_sed <имя> <файл в крейте> <выражение sed>: каждую версию крейта из
# Cargo.lock — копией с правкой (метка `freeos` в файле — правка уже есть).
crate_sed() {
    name=$1; file=$2; expr=$3
    for v in $(awk -v n="$name" '$0 == "name = \"" n "\"" { getline; gsub(/version = |"/, ""); print }' Cargo.lock); do
        dst=/cache/src/$name-$v
        if ! grep -q freeos "$dst/$file" 2>/dev/null; then
            rm -rf "$dst"
            mkdir -p /cache/src
            curl -sSfL "https://static.crates.io/crates/$name/$name-$v.crate" | tar -xz -C /cache/src
            sed -i "$expr" "$dst/$file"
            grep -q freeos "$dst/$file" || { echo "patch-craft-crates: правка $name $v не легла"; exit 1; }
        fi
        echo "$name-$(echo $v | tr . -) = { path = \"$dst\", package = \"$name\" }" >> /tmp/patches.toml
        echo "patch-craft-crates: $name $v"
    done
}

# filetime (tar/zip у craft-update): у FreeOS нет utimensat/lutimes — ветка
# wasm («не поддерживается»): время файлов при распаковке не ставится.
crate_sed filetime src/lib.rs 's/if #\[cfg(all(target_family = "wasm", not(target_os = "emscripten")))\]/if #[cfg(any(target_os = "freeos", all(target_family = "wasm", not(target_os = "emscripten"))))]/'

# getrandom 0.3 (craft-update, tempfile…): как у Vita — getentropy().
if grep -q '^version = "0.3.4"' Cargo.lock && grep -B1 '^version = "0.3.4"' Cargo.lock | grep -q 'name = "getrandom"'; then
    like_os getrandom 0.3.4 vita
fi
