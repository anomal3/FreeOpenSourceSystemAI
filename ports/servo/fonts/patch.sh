#!/bin/sh
# Шрифты Servo на FreeOS (N5, 08.11.2026): копии servo-fonts и
# servo-fonts-traits с freeos в условиях по ОС — FreeType (как Linux/Android),
# список шрифтов — свой (ports/servo/fonts/font_list.rs: обход /usr/share/fonts
# и таблицы name/OS2 через skrifa, без Fontconfig). Подключать после
# servo-env.sh (нужна copy_crate из patch-crates.sh); дописывает подмены в
# /tmp/patches.toml.
set -e
FONTS=servo-fonts-0.8.0
TRAITS=servo-fonts-traits-0.8.0

if copy_crate $TRAITS font_identifier.rs; then
    python3 - /cache/src/$TRAITS <<'EOF'
import sys
root = sys.argv[1]
p = root + '/font_identifier.rs'
s = open(p, encoding='utf-8').read()
old = '#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]\nmod platform {'
assert old in s, p
s = s.replace(old, '// FreeOS (__freeos__): FreeType, как Linux.\n#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "freeos"))]\nmod platform {', 1)
open(p, 'w', encoding='utf-8').write(s)
EOF
fi

if copy_crate $FONTS platform/freetype/mod.rs; then
    python3 - /cache/src/$FONTS <<'EOF'
import sys
root = sys.argv[1]
def edit(rel, pairs):
    p = root + '/' + rel
    s = open(p, encoding='utf-8').read()
    for old, new, count in pairs:
        n = s.count(old)
        assert n == count, (p, old, n)
        s = s.replace(old, new)
    open(p, 'w', encoding='utf-8').write(s)
# FreeType, HarfBuzz с FreeType и учёт памяти — как у Linux.
edit('Cargo.toml', [(
    'target_os = "linux", target_os = "android", target_os = "freebsd"))\'.dependencies.',
    'target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "freeos"))\'.dependencies.', 3)])
edit('platform/mod.rs', [(
    '#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]',
    '#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "freeos"))]', 2)])
edit('platform/freetype/mod.rs', [(
    'mod library_handle;',
    '// FreeOS (__freeos__): свой список шрифтов — обход каталогов, без Fontconfig.\n'
    '#[cfg(target_os = "freeos")]\nmod freeos {\n    pub mod font_list;\n}\n'
    '#[cfg(target_os = "freeos")]\npub use self::freeos::font_list;\n\nmod library_handle;', 1)])
EOF
    mkdir -p /cache/src/$FONTS/platform/freetype/freeos
    cp /work/ports/servo/fonts/font_list.rs /cache/src/$FONTS/platform/freetype/freeos/font_list.rs
fi
# Свежий font_list.rs — всегда (правят его в ports/servo/fonts).
cp /work/ports/servo/fonts/font_list.rs /cache/src/$FONTS/platform/freetype/freeos/font_list.rs

cat >> /tmp/patches.toml <<EOF
servo-fonts = { path = "/cache/src/$FONTS" }
servo-fonts-traits = { path = "/cache/src/$TRAITS" }
EOF
echo "fonts: $FONTS, $TRAITS (freeos) -> /cache/src"
