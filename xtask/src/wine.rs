//! Wine для программ Windows (фаза 71j): сборка WineHQ 11.0 для Debian 12 и
//! библиотеки, которые она загружает, — пакетом `wine-11.fpk` (`/opt/wine`
//! после `pkg install`).
//!
//! # Почему WineHQ, а не Debian
//!
//! В bookworm Wine 8.0 — без драйвера Wayland (`winewayland` появился в 9.0),
//! а X11 в системе нет. WineHQ собирает свежий Wine под bookworm — та же
//! glibc 2.36, что в корне `debian-12.fpk`. Берутся два пакета: `wine-stable`
//! (загрузчик `bin/wine`, `wineserver`) и `wine-stable-amd64` (64-битные
//! `ntdll.so`, `win32u.so`, `winewayland.so` и PE-библиотеки Windows).
//! 32-битная часть (`wine-stable-i386`) не нужна: только 64-битные программы.
//!
//! # Что кладётся
//!
//! Раскладка WineHQ сохраняется (`bin/`, `lib/wine/x86_64-unix`,
//! `lib/wine/x86_64-windows`, `share/wine`): загрузчик ищет `ntdll.so` по
//! пути от себя. Без библиотек импорта (`*.a`) и без модулей Unix, которым
//! нужно то, чего в системе нет (X11, ALSA, PulseAudio, GStreamer, USB,
//! udev, сканеры, камеры): Wine переживает отсутствие модуля, а `winex11.so`
//! без X11 всё равно не поднимется — графика сразу идёт в `winewayland`.
//! У PE-библиотек снимается отладочная информация (`llvm-strip
//! --strip-debug`): 767 МиБ → около 500.
//!
//! Библиотеки Debian, которые грузят модули Unix (`DT_NEEDED`) или открывает
//! `win32u` сам (FreeType — шрифты), — в `/opt/wine/lib`
//! (`LD_LIBRARY_PATH`), минус то, что уже есть в корне Debian.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::chromium::needed;
use crate::debian::{Node, follow, unpack, unpack_from};
use crate::{cbuild, sdk};

/// Имя пакета.
pub const PACKAGE_FILE: &str = "wine-11.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Архив пакетов WineHQ для Debian.
const WINEHQ: &str = "https://dl.winehq.org/wine-builds/debian/";

/// Главный пакет (64-битный Wine) — для списка `thirdparty`; хеши выписаны
/// из `dists/bookworm/main/binary-amd64/Packages.gz` WineHQ.
pub const MAIN_URL: &str = "https://dl.winehq.org/wine-builds/debian/pool/main/w/wine/wine-stable-amd64_11.0.0.0~bookworm-1_amd64.deb";
pub const MAIN_SHA256: &str = "e9c3d15171cd8d3bb164d742cdab7af43c0159dd8a91cd7ef237c7e5c8259675";

/// Пакеты WineHQ: путь в архиве и SHA-256 из `Packages` bookworm.
const WINE_PACKAGES: [(&str, &str); 2] = [
    ("pool/main/w/wine/wine-stable_11.0.0.0~bookworm-1_amd64.deb", "9d7d50a25006af1793901369bb2119d5810389c0cb74c56142b27de44135835d"),
    ("pool/main/w/wine/wine-stable-amd64_11.0.0.0~bookworm-1_amd64.deb", "e9c3d15171cd8d3bb164d742cdab7af43c0159dd8a91cd7ef237c7e5c8259675"),
];

/// Пакеты Debian (снимок 20261020, `Packages` bookworm): что нужно модулям
/// Unix, которые кладутся, и FreeType.
const PACKAGES: [(&str, &str); 15] = [
    // `ntdll.so`: раскрутка стека.
    ("pool/main/libu/libunwind/libunwind8_1.6.2-3_amd64.deb", "7b297868682836e4c87be349f17e4a56bc287586e3576503e84a5cb5485ce925"),
    ("pool/main/x/xz-utils/liblzma5_5.4.1-1+deb12u1_amd64.deb", "f96d8876b53ec89d76a992bc199679b818cf518225a768954d9451fb556a4eb7"),
    // `winewayland.so`.
    ("pool/main/w/wayland/libwayland-client0_1.21.0-1_amd64.deb", "1f002d028b8b79eec9847c636d8886d10dfe8c884cc2bebe18086b1391c5a28d"),
    ("pool/main/w/wayland/libwayland-egl1_1.21.0-1_amd64.deb", "cb4225f0e15e00a3570d4cbc288c87ff477d73e741c3f1c5a6d47cf350d45378"),
    ("pool/main/libf/libffi/libffi8_3.4.4-1_amd64.deb", "6d9f6c25c30efccce6d4bceaa48ea86c329a3432abb360a141f76ac223a4c34a"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.5.0-1_amd64.deb", "e3fe045b9a33a101de1c5a912a4a10928db055c3f68930f47eccbb44d7c7d54e"),
    ("pool/main/libx/libxkbcommon/libxkbregistry0_1.5.0-1_amd64.deb", "5324e265dffb33dd0b8d4d8e8730bd459ad3476d80035077fee8eaffa551e4f7"),
    ("pool/main/libx/libxml2/libxml2_2.9.14+dfsg-1.3~deb12u6_amd64.deb", "4460e39dda10a815881374217cde08474747cfa018358cd8612c14b390eff53b"),
    ("pool/main/i/icu/libicu72_72.1-3+deb12u1_amd64.deb", "f7f6f99c6d7b025914df2447fc93e11d22c44c0c8bdd8b6f36691c9e7ddcef88"),
    // `win32u.so` открывает FreeType сам (`dlopen`): без него нет текста.
    ("pool/main/f/freetype/libfreetype6_2.12.1+dfsg-5+deb12u4_amd64.deb", "8043e479f73f29992d652e3f9dfe8b17f9780c7ea6330afe379ec5f9f188ac44"),
    ("pool/main/libp/libpng1.6/libpng16-16_1.6.39-2+deb12u5_amd64.deb", "a56d64bfaa9da12aafb83347909e62e6fd5fd251e6b34c194065911a30359978"),
    ("pool/main/b/brotli/libbrotli1_1.0.9-2+b6_amd64.deb", "563b4caec1aa5e876bd3355b36e7a38e1484baf5a293b48d1e8bd22db786e4d7"),
    ("pool/main/b/bzip2/libbz2-1.0_1.0.8-5+b1_amd64.deb", "54149da3f44b22d523b26b692033b84503d822cc5122fed606ea69cc83ca5aeb"),
    ("pool/main/e/expat/libexpat1_2.5.0-1+deb12u2_amd64.deb", "2255e62fc22a86d2c544b8a3f516da9aee19383ad5742722ab4ce7f66a30dbc8"),
    // Fontconfig (`dlopen`): без него `win32u` Wine 11 зовёт `FcPatternCreate`
    // по пустому указателю в конце `gdi_init` — `user32` не доходит до
    // станции окон, и `explorer /desktop` плодится без конца (06.11).
    ("pool/main/f/fontconfig/libfontconfig1_2.14.1-4_amd64.deb", "16ee38d374e064f534116dc442b086ef26f9831f1c0af7e5fb4fe4512e700649"),
];

/// Корень Wine в пакетах WineHQ.
const PREFIX: &str = "opt/wine-stable/";

/// Где ищет `ld.so` (корень Debian 12, без usrmerge).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// Модули Unix, которые не кладутся: им нужно то, чего в системе нет.
///
/// И `wine-preloader`: он заранее занимает нижние адреса (образы Windows,
/// `KUSER_SHARED_DATA`), пока туда не легли библиотеки, — у нас `ld.so` и
/// `mmap` без подсказки и так кладут всё в окно выше 512 ГиБ. К тому же он
/// статический PIE без метки ABI и без `PT_INTERP`, и ядро не узнаёт в нём
/// программу Linux. Без него загрузчик запускает `x86_64-unix/wine` сам.
const SKIP_UNIX: [&str; 11] = [
    "wine-preloader",
    "winex11.so",
    "winealsa.so",
    "winepulse.so",
    "winegstreamer.so",
    "wineusb.so",
    "winebus.so",
    "gphoto2.so",
    "sane.so",
    "opencl.so",
    "wpcap.so",
];

/// Что `dlopen`-ит Wine сам и что должно быть в пакете.
const DLOPEN: [&str; 2] = ["libfreetype.so.6", "libfontconfig.so.1"];

/// `fonts.conf` для fontconfig: шрифты Wine, кеш — во временном каталоге (в
/// корне Debian нет ни `/etc/fonts`, ни шрифтов).
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/wine/share/wine/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    </fontconfig>\n";

/// Скачать пакеты и собрать `wine-11.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut deps: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut deps)?;
    let mut wine: BTreeMap<String, Node> = BTreeMap::new();
    unpack_from(WINEHQ, &cache, &WINE_PACKAGES, &mut wine)?;

    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    let mut todo: Vec<String> = DLOPEN.iter().map(|name| String::from(*name)).collect();
    let mut to_strip: Vec<(String, Vec<u8>, u32)> = Vec::new();
    for (path, node) in &wine {
        let Some(rest) = path.strip_prefix(PREFIX) else { continue };
        if !matches!(node, Node::File { .. }) || rest.ends_with(".a") || rest.starts_with("share/man/") {
            continue;
        }
        // Инструменты сборки Winelib в системе не нужны.
        if ["bin/widl", "bin/winebuild", "bin/winedump", "bin/winegcc", "bin/winemaker", "bin/wmc", "bin/wrc", "bin/function_grep.pl"].contains(&rest) {
            continue;
        }
        if rest.starts_with("share/applications/") {
            continue;
        }
        let Some((data, mode)) = follow(&wine, path, 0) else { continue };
        if let Some(name) = rest.strip_prefix("lib/wine/x86_64-unix/") {
            if SKIP_UNIX.contains(&name) {
                continue;
            }
            if name.ends_with(".so") {
                todo.extend(needed(data).with_context(|| format!("{name}: не разобран ELF"))?);
            }
        }
        if rest == "bin/wine" || rest == "bin/wineserver" {
            todo.extend(needed(data).with_context(|| format!("{rest}: не разобран ELF"))?);
        }
        if rest.starts_with("lib/wine/x86_64-windows/") && data.starts_with(b"MZ") {
            to_strip.push((String::from(rest), data.clone(), mode));
        } else {
            files.push((String::from(rest), data.clone(), mode));
        }
    }
    if !files.iter().any(|(path, _, _)| path == "lib/wine/x86_64-unix/winewayland.so") {
        bail!("в пакетах WineHQ нет winewayland.so");
    }

    // Библиотеки по `DT_NEEDED`: свои модули Wine (`ntdll.so`, `win32u.so`)
    // лежат рядом, корень Debian — уже в системе, остальное — из `PACKAGES`.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut from_base = 0;
    let mut libs = 0;
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if name == "ntdll.so" || name == "win32u.so" {
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        let Some((data, mode)) = LIB_DIRS.iter().find_map(|dir| follow(&deps, &format!("{dir}/{name}"), 0)) else {
            bail!("библиотеки {name} нет ни в корне Debian, ни в пакетах Wine");
        };
        todo.extend(needed(data).with_context(|| format!("{name}: не разобран ELF"))?);
        files.push((format!("lib/{name}"), data.clone(), mode));
        libs += 1;
    }
    say!("wine: {libs} библиотек Debian в пакет, {from_base} уже в корне Debian");

    let stripped = strip_pe(&to_strip)?;
    files.extend(stripped);

    // Запуск — сценарий bash: библиотеки рядом (`LD_LIBRARY_PATH`), окна —
    // через сервер Wayland ядра, префикс — в доме того, кто вошёл.
    // `WINEDLLOVERRIDES`: Mono и Gecko при создании префикса Wine предлагает
    // скачать окном — их нет и не надо.
    let script = "#!/bin/bash\n\
        # Wine 11 (фаза 71j): программы Windows в окнах стола через Wayland.\n\
        export LD_LIBRARY_PATH=/opt/wine/lib\n\
        export FONTCONFIG_FILE=/opt/wine/share/wine/fonts.conf\n\
        export XDG_RUNTIME_DIR=/run/user/0\n\
        export WAYLAND_DISPLAY=wayland-0\n\
        unset DISPLAY\n\
        export HOME=${HOME:-/root}\n\
        export TMPDIR=${TMPDIR:-$HOME}\n\
        export WINEPREFIX=${WINEPREFIX:-$HOME/.wine}\n\
        export WINEDLLOVERRIDES=\"mscoree,mshtml=${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}\"\n\
        export WINEDEBUG=${WINEDEBUG:-fixme-all}\n\
        # `--debug=КАНАЛЫ` первым словом — свой WINEDEBUG (у `run` оболочки\n\
        # нет переменных окружения).\n\
        case \"$1\" in --debug=*) export WINEDEBUG=\"${1#--debug=}\"; shift ;; esac\n\
        # `--server-debug=N` — заранее поднять wineserver с журналом запросов.\n\
        case \"$1\" in --server-debug=*) mkdir -p \"$WINEPREFIX\"; /opt/wine/bin/wineserver -d\"${1#--server-debug=}\"; shift ;; esac\n\
        [ $# -eq 0 ] && set -- notepad\n\
        exec /opt/wine/bin/wine \"$@\"\n";
    files.push((String::from("bin/fossai-wine"), script.as_bytes().to_vec(), 0o755));
    files.push((String::from("share/wine/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));

    let mut builder = Builder::new(Kind::Package, "wine", "11");
    builder.field("summary", "Wine 11.0 (WineHQ build for Debian 12), 64-bit; windows through the kernel Wayland server (winewayland)");
    builder.field("about", "Wine для программ Windows: Блокнот и другие в окнах стола");
    builder.field("caption", "Notepad (Wine)");
    builder.field("start", "/opt/debian/bin/bash /opt/wine/bin/fossai-wine notepad");
    builder.field("permissions", "windows files net jit");
    builder.requires("debian");
    let mut bytes = 0usize;
    for (path, data, mode) in &files {
        bytes += data.len();
        builder.file(&Entry { path: path.clone(), mode: (*mode & 0o7777) as u16, uid: 0, gid: 0, data: data.clone() });
    }
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!("wine: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// Снять отладочную информацию с PE-библиотек (`llvm-strip --strip-debug`,
/// пачками — одна команда на сотню файлов).
fn strip_pe(input: &[(String, Vec<u8>, u32)]) -> Result<Vec<(String, Vec<u8>, u32)>> {
    let strip = cbuild::llvm_tool("llvm-strip")?;
    let work = sdk::root().join("wine/strip");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let before: usize = input.iter().map(|(_, data, _)| data.len()).sum();
    let mut out = Vec::with_capacity(input.len());
    for (chunk_index, chunk) in input.chunks(100).enumerate() {
        let mut names = Vec::new();
        for (index, (_, data, _)) in chunk.iter().enumerate() {
            let file = work.join(format!("{chunk_index}-{index}.bin"));
            fs::write(&file, data)?;
            names.push(file);
        }
        let status = std::process::Command::new(&strip)
            .arg("--strip-debug")
            .args(&names)
            .status()
            .context("не удалось запустить llvm-strip")?;
        if !status.success() {
            bail!("llvm-strip: {status}");
        }
        for ((path, _, mode), file) in chunk.iter().zip(&names) {
            out.push((path.clone(), fs::read(file)?, *mode));
        }
    }
    let _ = fs::remove_dir_all(&work);
    let after: usize = out.iter().map(|(_, data, _)| data.len()).sum();
    say!("wine: PE {} файлов, {} → {} МиБ без отладки", out.len(), before >> 20, after >> 20);
    Ok(out)
}
