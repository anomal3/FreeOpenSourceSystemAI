//! Proton для программ и игр Windows (12.11.2026): готовая сборка GE-Proton и
//! библиотеки, которые её модули грузят, — пакетом `proton-11.fpk`
//! (`/opt/proton` после `pkg install`). Без Steam и без его среды исполнения
//! (Steam Runtime, pressure-vessel): `bin/fossai-proton` ставит окружение сам.
//!
//! # Что берётся из архива
//!
//! Каталог `files/` GE-Proton как есть: `bin/wine`, `bin/wineserver`,
//! `lib/wine/x86_64-unix` (модули Unix, без X11/звука/USB — как у `wine.rs`),
//! `lib/wine/{x86_64,i386}-windows` (PE-библиотеки, 32-битные — для нового
//! WoW64, шаг 4 плана), DXVK и VKD3D-Proton, `share/wine`. Не берётся:
//! `lib/wine/i386-unix` и `lib/i386-linux-gnu` (32-битный код Linux — его ядро
//! не исполняет; 32-битные программы Windows пойдут новым WoW64 внутри
//! 64-битного процесса), `share/default_pfx` (шаблон префикса Steam: префикс
//! создаёт `wineboot`), `wine-preloader` (см. `wine.rs`).
//!
//! # Свои зависимости
//!
//! Модули Unix GE-Proton собраны в Steam Runtime и ждут от него библиотек.
//! Часть лежит в архиве (`files/lib/x86_64-linux-gnu`), остальное — по
//! замыканию `DT_NEEDED`: что есть в корне Debian 12 — берётся оттуда, чего
//! нет — из пакетов Debian (`wine::PACKAGES` и свои ниже). Не нашлось —
//! сборка останавливается со списком: молча собранный пакет с дырой
//! выглядел бы потом как «Wine не стартует».

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::chromium::needed;
use crate::debian::{Node, follow, unpack};
use crate::java::{Item, stream_tar};
use crate::{cbuild, sdk};

/// Имя пакета.
pub const PACKAGE_FILE: &str = "proton-11.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Включён ли Proton в носитель и цепочки стенда: архив в полгигабайта и
/// пакет больше гигабайта — только по `FOSSAI_WITH_PROTON=1` (как Rider).
pub fn enabled() -> bool {
    std::env::var("FOSSAI_WITH_PROTON").is_ok_and(|value| value == "1")
}

/// GE-Proton11-12 (03.11.2026): адрес и SHA-256 (SHA-512 сверен с
/// опубликованным `GE-Proton11-12-x86_64.sha512sum`).
pub const URL: &str =
    "https://github.com/GloriousEggroll/proton-ge-custom/releases/download/GE-Proton11-12/GE-Proton11-12-x86_64.tar.gz";
pub const SHA256: &str = "0595159e417c3dbbfb7cc3ae0d3ed15a617cc607aa23ef756ea79029d7af5d70";

/// Корень внутри архива.
const ROOT: &str = "GE-Proton11-12-x86_64/files/";

/// Что из `files/` не кладётся (префиксы путей).
const SKIP_PREFIXES: [&str; 6] = [
    "lib/wine/i386-unix/",
    // NVML (сведения о видеокартах NVIDIA) — без драйвера NVIDIA не нужен.
    "lib/wine/nvidia-libs/",
    "lib/i386-linux-gnu/",
    "share/default_pfx/",
    "share/man/",
    "bin/xrandr",
];

/// Модули Unix, которые не кладутся (как `wine::SKIP_UNIX`), и модули
/// Proton, которым нужно то, чего в системе нет:
/// * `winedmo.so` — видео через FFmpeg (VA-API, VDPAU, DRM, X11) — позже;
/// * `winscard.so` — смарт-карты (pcsclite);
/// * `amd_ags_x64.so` — служба видеокарт AMD (`libdrm_amdgpu`);
/// * `lsteamclient.so`, `vrclient*.so`, `wineopenxr.so` — Steam и VR;
/// * `protontts.so`, `windows.media.speech.so` — речь (espeak, vosk, onnx);
/// * `qcap.so`, `avicap32.so` — камеры, `winebth.so` — Bluetooth (D-Bus).
const SKIP_UNIX: &[&str] = &[
    "wine-preloader",
    "wine64-preloader",
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
    "winedmo.so",
    "winscard.so",
    "amd_ags_x64.so",
    "lsteamclient.so",
    "vrclient.so",
    "vrclient_x64.so",
    "wineopenxr.so",
    "protontts.so",
    "windows.media.speech.so",
    "qcap.so",
    "avicap32.so",
    "winebth.so",
];

/// Что `dlopen`-ит Wine сам и что должно быть: шрифты (как у `wine.rs`) и
/// GnuTLS — HTTPS и сертификаты программ Windows (`bcrypt`, `secur32`).
const DLOPEN: [&str; 3] = ["libfreetype.so.6", "libfontconfig.so.1", "libgnutls.so.30"];

/// Свои пакеты Debian (снимок 20261020, `Packages` bookworm) сверх
/// `wine::PACKAGES`: GnuTLS и то, на чём он стоит.
const PACKAGES: [(&str, &str); 8] = [
    ("pool/main/g/gnutls28/libgnutls30_3.7.9-2+deb12u7_amd64.deb", "30abec8c824feb1d2d7e9000a34083cccd19d139625e1b21547e3ac53b922f8e"),
    ("pool/main/g/gmp/libgmp10_6.2.1+dfsg1-1.1_amd64.deb", "187aedef2ed763f425c1e523753b9719677633c7eede660401739e9c893482bd"),
    ("pool/main/libi/libidn2/libidn2-0_2.3.3-1+b1_amd64.deb", "d50716d5824083d667427817d506b45d3f59dc77e1ca52de000f3f62d4918afa"),
    ("pool/main/libt/libtasn1-6/libtasn1-6_4.19.0-2+deb12u1_amd64.deb", "2c27c613b69be248d09432839ea3d3382f6ee27fabaa65987aa2d884cd00afda"),
    ("pool/main/libu/libunistring/libunistring2_1.0-2_amd64.deb", "d466bbfe011d764d793c1d9d777cad9c7cf65b938e11598f27408171ad95a951"),
    ("pool/main/n/nettle/libhogweed6_3.8.1-2_amd64.deb", "ed8185c28b2cb519744a5a462dcd720d3b332c9b88a1d0002eac06dc8550cb94"),
    ("pool/main/n/nettle/libnettle8_3.8.1-2_amd64.deb", "45922e6e289ffd92f0f92d2bb9159e84236ff202d552a461bf10e5335b3f0261"),
    ("pool/main/p/p11-kit/libp11-kit0_0.24.1-2_amd64.deb", "251330faddbf013f060fcdb41f4b0c037c8a6e89ba7c09b04bfcc4e3f0807b22"),
];

/// Библиотеки GE-Proton в архиве.
const BUNDLED: &str = "lib/x86_64-linux-gnu/";

/// Где ищет `ld.so` в корне Debian 12.
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// `fonts.conf`: шрифты Proton, кеш — во временном каталоге.
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/proton/share/wine/fonts</dir>\n\
    \t<dir>/opt/proton/share/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    </fontconfig>\n";

/// Скачать GE-Proton и собрать `proton-11.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let archive = crate::java::cached_download(URL, SHA256)?;
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut deps: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::wine::PACKAGES, &mut deps)?;
    unpack(&cache, &PACKAGES, &mut deps)?;

    // Всё нужное из архива — в память (без пропущенного), ссылки — отдельно.
    let mut taken: BTreeMap<String, (Vec<u8>, u32)> = BTreeMap::new();
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    let keep = |rest: &str| {
        if SKIP_PREFIXES.iter().any(|prefix| rest.starts_with(prefix)) {
            return false;
        }
        if let Some(name) = rest.strip_prefix("lib/wine/x86_64-unix/") {
            return !SKIP_UNIX.contains(&name);
        }
        true
    };
    stream_tar(
        &archive,
        |path| path.strip_prefix(ROOT).is_some_and(keep),
        |path, item| {
            let Some(rest) = path.strip_prefix(ROOT) else { return };
            match item {
                Item::File { data, mode } => {
                    taken.insert(rest.to_string(), (data, mode));
                }
                Item::Link(target) => {
                    links.insert(rest.to_string(), target);
                }
            }
        },
    )?;
    if !taken.contains_key("lib/wine/x86_64-unix/winewayland.so") {
        bail!("в GE-Proton нет lib/wine/x86_64-unix/winewayland.so");
    }
    if !taken.contains_key("bin/wine") {
        bail!("в GE-Proton нет bin/wine");
    }

    // Ссылка внутри архива — в файл, на который она указывает (цепочки тоже).
    let resolve = |path: &str| -> Option<(Vec<u8>, u32)> {
        let mut at = path.to_string();
        for _ in 0..8 {
            if let Some(found) = taken.get(&at) {
                return Some(found.clone());
            }
            let target = links.get(&at)?;
            at = if target.starts_with('/') {
                return None;
            } else {
                let dir = at.rsplit_once('/').map_or("", |(dir, _)| dir);
                normalize(&format!("{dir}/{target}"))
            };
        }
        None
    };

    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    // Что искать и кто это потребовал — для внятного отказа.
    let mut todo: Vec<(String, String)> = DLOPEN.iter().map(|name| (String::from(*name), String::from("dlopen"))).collect();
    for (rest, (data, mode)) in &taken {
        // Библиотеки из архива кладутся только по `DT_NEEDED` — ниже.
        if rest.starts_with(BUNDLED) {
            continue;
        }
        let unix_module = rest.starts_with("lib/wine/x86_64-unix/") && rest.ends_with(".so");
        if unix_module || rest == "bin/wine" || rest == "bin/wineserver" {
            let by = rest.rsplit('/').next().unwrap_or(rest).to_string();
            todo.extend(needed(data).with_context(|| format!("{rest}: не разобран ELF"))?.into_iter().map(|name| (name, by.clone())));
        }
        files.push((rest.clone(), data.clone(), *mode));
    }
    // Ссылки вне библиотек (`msidb -> wine` и подобные) — копиями файла.
    for path in links.keys() {
        if path.starts_with(BUNDLED) || taken.contains_key(path) {
            continue;
        }
        if let Some((data, mode)) = resolve(path) {
            files.push((path.clone(), data, mode));
        }
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    let (mut bundled, mut from_base, mut from_deps) = (0, 0, 0);
    while let Some((name, by)) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if name == "ntdll.so" || name == "win32u.so" {
            continue;
        }
        let by_this = |list: Vec<String>| list.into_iter().map(|next| (next, name.clone())).collect::<Vec<_>>();
        if let Some((data, mode)) = resolve(&format!("{BUNDLED}{name}")) {
            todo.extend(by_this(needed(&data).with_context(|| format!("{name}: не разобран ELF"))?));
            files.push((format!("{BUNDLED}{name}"), data, mode));
            bundled += 1;
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        if let Some((data, mode)) = LIB_DIRS.iter().find_map(|dir| follow(&deps, &format!("{dir}/{name}"), 0)) {
            todo.extend(by_this(needed(data).with_context(|| format!("{name}: не разобран ELF"))?));
            files.push((format!("lib/{name}"), data.clone(), mode));
            from_deps += 1;
            continue;
        }
        missing.push(format!("{name} (нужна {by})"));
    }
    say!("proton: библиотек из архива {bundled}, из корня Debian {from_base}, из пакетов Debian {from_deps}");
    if !missing.is_empty() {
        bail!("proton: не нашлись библиотеки: {}", missing.join(", "));
    }

    let script = "#!/bin/bash\n\
        # GE-Proton (12.11.2026): программы и игры Windows без Steam, окна —\n\
        # через сервер Wayland ядра, синхронизация — fsync (futex_waitv).\n\
        P=/opt/proton\n\
        export LD_LIBRARY_PATH=$P/lib/x86_64-linux-gnu:$P/lib\n\
        export WINEDLLPATH=$P/lib/vkd3d:$P/lib/wine\n\
        export PATH=$P/bin:$PATH\n\
        export FONTCONFIG_FILE=$P/share/wine/fonts.conf\n\
        export XDG_RUNTIME_DIR=/run/user/0\n\
        export WAYLAND_DISPLAY=wayland-0\n\
        unset DISPLAY\n\
        export WINE_GRAPHICS_DRIVER=wayland\n\
        export HOME=${HOME:-/root}\n\
        export TMPDIR=${TMPDIR:-$HOME}\n\
        export WINEPREFIX=${WINEPREFIX:-$HOME/.proton}\n\
        # fsync — да (futex_waitv есть с 12.11), ntsync — нет (/dev/ntsync нет),\n\
        # esync — запасной (eventfd).\n\
        export WINEFSYNC=${WINEFSYNC:-1}\n\
        export WINEESYNC=${WINEESYNC:-1}\n\
        export WINENTSYNC=0\n\
        export WINEDLLOVERRIDES=\"mscoree,mshtml=${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}\"\n\
        export WINEDEBUG=${WINEDEBUG:-fixme-all}\n\
        case \"$1\" in --debug=*) export WINEDEBUG=\"${1#--debug=}\"; shift ;; esac\n\
        case \"$1\" in --server-debug=*) mkdir -p \"$WINEPREFIX\"; $P/bin/wineserver -d\"${1#--server-debug=}\"; shift ;; esac\n\
        [ $# -eq 0 ] && set -- notepad\n\
        exec $P/bin/wine \"$@\"\n";
    files.push((String::from("bin/fossai-proton"), script.as_bytes().to_vec(), 0o755));
    files.push((String::from("share/wine/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));

    let mut builder = Builder::new(Kind::Package, "proton", "11");
    builder.field("summary", "GE-Proton11-12 without Steam: Wine with fsync, DXVK and VKD3D-Proton; windows through the kernel Wayland server");
    builder.field("about", "Proton для игр и программ Windows");
    builder.field("start", "/opt/debian/bin/bash /opt/proton/bin/fossai-proton");
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
    say!("proton: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}

/// Убрать `.` и `..` из относительного пути.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}
