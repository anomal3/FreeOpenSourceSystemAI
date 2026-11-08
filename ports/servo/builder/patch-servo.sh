#!/bin/sh
# Правки FreeOS в крейтах самого Servo 0.8 (N5). Подключать после
# servo-env.sh (нужна copy_crate из patch-crates.sh): готовит копии в
# /cache/src и дописывает подмены в /tmp/patches.toml.

# gaol — песочница процессов, у servo и servo-constellation она под cfg по
# ОС (не Windows/Android/iOS/arm), а используется только с фичей
# `multiprocess`, которая у нас выключена (нет fork). Сама gaol ОС не знает
# (нет `platform::Operation`) — подменяем пустым крейтом той же версии.
mkdir -p /cache/src/gaol-stub/src
cat > /cache/src/gaol-stub/Cargo.toml <<'EOF'
[package]
name = "gaol"
version = "0.2.1"
edition = "2018"
description = "FreeOS: empty stand-in for gaol (sandboxing is only used by servo's multiprocess feature, which is off)"
EOF
echo '//! FreeOS: песочница gaol нужна только Servo с `multiprocess` (у нас выключен) — пусто.' > /cache/src/gaol-stub/src/lib.rs
echo 'gaol = { path = "/cache/src/gaol-stub" }' >> /tmp/patches.toml

# Шрифты: FreeType и свой список шрифтов (ports/servo/fonts).
. /work/ports/servo/fonts/patch.sh

# servo-script: `navigator.platform` — как у Linux («Linux»): сайты
# определяют по нему платформу, и незнакомое имя только мешает им.
SCRIPT=servo-script-0.8.0
if copy_crate $SCRIPT dom/navigator/navigatorinfo.rs; then
    python3 - /cache/src/$SCRIPT <<'EOF'
import sys
p = sys.argv[1] + '/dom/navigator/navigatorinfo.rs'
s = open(p, encoding='utf-8').read()
old = '#[cfg(any(target_os = "android", target_os = "linux", target_os = "freebsd"))]\npub(crate) fn Platform() -> DOMString {'
assert old in s, p
s = s.replace(old, '// FreeOS (__freeos__): как Linux — так сайтам понятнее.\n#[cfg(any(target_os = "android", target_os = "linux", target_os = "freebsd", target_os = "freeos"))]\npub(crate) fn Platform() -> DOMString {', 1)
open(p, 'w', encoding='utf-8').write(s)
EOF
fi
echo "servo-script = { path = \"/cache/src/$SCRIPT\" }" >> /tmp/patches.toml
echo "patch-servo: gaol (пусто), шрифты, $SCRIPT (freeos)"
