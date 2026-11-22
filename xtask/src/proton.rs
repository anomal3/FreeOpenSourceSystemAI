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
//! 64-битного процесса), `share/default_pfx` (шаблон префикса Steam под
//! X11: свой шаблон снимает сценарий `proton-prefix`, см. [`PREFIX_SNAPSHOT`]),
//! `wine-preloader` (см. `wine.rs`).
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
const SKIP_PREFIXES: [&str; 10] = [
    // Драйвер X11 целиком (12.11): без `winex11.so` его PE-часть всё равно
    // грузится, `explorer` выбирает x11 первым по списку и остаётся без
    // окна («no driver could be loaded») — до `wayland` дело не доходит.
    "lib/wine/x86_64-windows/winex11.drv",
    "lib/wine/i386-windows/winex11.drv",
    // wine-mono и Gecko: .NET — своим `clr-vm`, HTML-движок не нужен; в
    // скрипте они и так выключены (`mscoree,mshtml=`).
    "share/wine/mono/",
    "share/wine/gecko/",
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
    // `winealsa.so` кладётся: звук через ALSA ядра (`linux/snd.rs`, 12.11).
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

/// glibc Debian 13 (trixie, тот же снимок): модули Unix GE-Proton11 собраны
/// под glibc 2.38+ (`ntdll.so`: «version GLIBC_2.38 not found»), а в корне
/// Debian 12 — 2.36. Своя glibc кладётся в `glibc/`, а в ELF-заголовках
/// `wine` и `wineserver` загрузчик (`PT_INTERP`) переписывается на её
/// `ld.so` — см. [`patch_interp`].
const GLIBC: [(&str, &str); 1] = [(
    "pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb",
    "967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9",
)];

/// Библиотеки из Debian 13 поверх Debian 12 (12.11): `winewayland.so` GE
/// зовёт `wl_display_create_queue_with_name` (libwayland 1.23) и
/// `xkb_compose_table_iterator_*` (libxkbcommon 1.6) — с библиотеками
/// bookworm `dlopen` драйвера молча падал, и `explorer` оставался без окна.
/// Раскладываются после `wine::PACKAGES` и перекрывают их.
const TRIXIE: [(&str, &str); 4] = [
    ("pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb", "d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33"),
    ("pool/main/w/wayland/libwayland-egl1_1.23.1-3_amd64.deb", "7c5c3429028b39b9432792db08cb218c43891df439ebc648b83ebee676a80ec5"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.7.0-2_amd64.deb", "f75ee544f55acc6a271debfab3ea4ae0458afc89d81cfe1a71137e07d4895b86"),
    ("pool/main/libx/libxkbcommon/libxkbregistry0_1.7.0-2_amd64.deb", "e9b5a7051efa2b299348b4f6b1ebc1416f2523c4224af495d9cca4adbb22183a"),
];

/// Раскладки клавиатуры Xkb (bookworm, `all`).
const XKB_DATA: [(&str, &str); 1] =
    [("pool/main/x/xkeyboard-config/xkb-data_2.35.1-1_all.deb", "28a79c61b785f403da93af58c39aed13c022032935e3e54362c6707169cfe982")];

/// Что из glibc trixie кладётся (`usr/lib/x86_64-linux-gnu/…`).
const GLIBC_LIBS: [&str; 12] = [
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libmvec.so.1",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libresolv.so.2",
    "libutil.so.1",
    "libanl.so.1",
    "libnss_files.so.2",
    "libnss_dns.so.2",
];

/// Загрузчик Debian 12 в `PT_INTERP` и наш на его место: не длиннее, чтобы
/// заменить строку на месте (хвост — нули).
const INTERP_OLD: &[u8] = b"/lib64/ld-linux-x86-64.so.2\0";
const INTERP_NEW: &[u8] = b"/opt/proton/glibc/ld.so\0";

/// Переписать загрузчик программы на свою glibc. `false` — строки нет.
fn patch_interp(data: &mut [u8]) -> bool {
    let Some(at) = data.windows(INTERP_OLD.len()).position(|window| window == INTERP_OLD) else {
        return false;
    };
    data[at..at + INTERP_OLD.len()].fill(0);
    data[at..at + INTERP_NEW.len()].copy_from_slice(INTERP_NEW);
    true
}

/// Имя снимка шаблона префикса (`build/proton-prefix/win64.*`). Шаблон один
/// на `WINEARCH=win64` (`fossai-proton`) и `wow64` (`bin/tmnf`): `wineboot`
/// GE строит их одинаково — `#arch=win64` и `syswow64` у обоих, разница
/// только во временах ключей реестра (сверено 17.11).
const PREFIX_SNAPSHOT: &str = "win64";

/// Снимок шаблона префикса на хосте: `build/proton-prefix/<имя>.list` и
/// `.blob`. Список — строки через табуляцию: `d путь права`, `l путь цель`,
/// `f путь права размер`; содержимое файлов — подряд в `.blob`, в порядке
/// строк `f`. Не tar и не дерево каталогов: имена префикса Wine длиннее
/// MAX_PATH и различаются регистром, NTFS хоста их не держит.
fn template_paths(name: &str) -> (PathBuf, PathBuf) {
    let dir = crate::paths::build_dir().join("proton-prefix");
    (dir.join(format!("{name}.list")), dir.join(format!("{name}.blob")))
}

/// Снять шаблон префикса с раздела состояния диска стенда (шаг `Fetch` сценария
/// `proton-prefix`, гость уже погас): каталог `guest_dir` целиком и его
/// символические ссылки из таблицы ядра (`/etc/linux-links`).
pub fn save_template(disk: &std::path::Path, guest_dir: &str, name: &str) -> Result<()> {
    let Some(tree) = crate::statefs::read_tree(disk, guest_dir)? else {
        bail!("у гостя нет /{guest_dir}");
    };
    let mut list = String::new();
    let mut blob: Vec<u8> = Vec::new();
    for node in &tree {
        if node.path.contains(['\t', '\n']) {
            bail!("имя с табуляцией или переводом строки: {:?}", node.path);
        }
        match &node.data {
            None => list.push_str(&format!("d\t{}\t{:o}\n", node.path, node.mode)),
            Some(data) => {
                list.push_str(&format!("f\t{}\t{:o}\t{}\n", node.path, node.mode, data.len()));
                blob.extend_from_slice(data);
            }
        }
    }
    let table = crate::statefs::read_file(disk, "etc/linux-links")?.unwrap_or_default();
    let base = format!("/{}/", guest_dir.trim_matches('/'));
    let mut links = 0;
    for line in String::from_utf8_lossy(&table).lines() {
        let Some((path, target)) = line.split_once('\t') else { continue };
        if let Some(rel) = path.strip_prefix(&base) {
            list.push_str(&format!("l\t{rel}\t{target}\n"));
            links += 1;
        }
    }
    let (list_path, blob_path) = template_paths(name);
    fs::create_dir_all(list_path.parent().unwrap())?;
    fs::write(&list_path, list)?;
    fs::write(&blob_path, &blob)?;
    say!(
        "proton: шаблон {name} с /{guest_dir}: {} узлов, ссылок {links}, {} МиБ -> {}",
        tree.len(),
        blob.len() / (1024 * 1024),
        list_path.display()
    );
    Ok(())
}

/// Шаблон префикса в пакет: файлы — `share/fossai/prefix/…`, пустые каталоги
/// и ссылки (их у fpk нет) — списком `share/fossai/prefix.list` (`d путь`,
/// `l путь цель`), его разбирает `fossai-proton`. `false` — снимка нет.
fn add_template(name: &str, files: &mut Vec<(String, Vec<u8>, u32)>) -> Result<bool> {
    let (list_path, blob_path) = template_paths(name);
    let (Ok(list), Ok(blob)) = (fs::read_to_string(&list_path), fs::read(&blob_path)) else {
        return Ok(false);
    };
    let root = String::from("share/fossai/prefix");
    let mut extra = String::new();
    let mut parents: BTreeSet<String> = BTreeSet::new();
    let mut dirs: Vec<String> = Vec::new();
    let (mut at, mut count) = (0usize, 0usize);
    for line in list.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["d", path, _] => dirs.push((*path).to_string()),
            ["l", path, target] => extra.push_str(&format!("l\t{path}\t{target}\n")),
            ["f", path, mode, size] => {
                let size: usize = size.parse().with_context(|| format!("{}: {line}", list_path.display()))?;
                let mode = u32::from_str_radix(mode, 8).unwrap_or(0o644);
                let data = blob.get(at..at + size).with_context(|| format!("{}: короче списка", blob_path.display()))?;
                at += size;
                // Каталоги файла — уже не пустые.
                let mut dir = *path;
                while let Some((up, _)) = dir.rsplit_once('/') {
                    parents.insert(up.to_string());
                    dir = up;
                }
                files.push((format!("{root}/{path}"), data.to_vec(), mode | 0o600));
                count += 1;
            }
            _ => bail!("{}: непонятная строка {line:?}", list_path.display()),
        }
    }
    // Пустые каталоги — `mkdir` в скрипте (первыми: в них ложатся ссылки).
    let mut empty: String = dirs.iter().filter(|dir| !parents.contains(*dir)).map(|dir| format!("d\t{dir}\n")).collect();
    empty.push_str(&extra);
    files.push((format!("{root}.list"), empty.into_bytes(), 0o644));
    say!("proton: шаблон префикса {name} — {count} файлов, {} МиБ", at / (1024 * 1024));
    Ok(true)
}

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
    // Пути в trixie — `usr/lib/…` (usrmerge), как и у этих пакетов bookworm:
    // свежие версии ложатся поверх.
    unpack(&cache, &TRIXIE, &mut deps)?;

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
    // Раскладки клавиатуры (xkb-data): без них `winewayland` не разбирает
    // правила Xkb («Failed to parse default Xkb ruleset»). Корень —
    // `XKB_CONFIG_ROOT` в скрипте.
    let mut xkb: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &XKB_DATA, &mut xkb)?;
    let xkb_paths: Vec<String> = xkb.keys().filter(|path| path.starts_with("usr/share/X11/xkb/")).cloned().collect();
    for path in xkb_paths {
        if let Some((data, mode)) = follow(&xkb, &path, 0) {
            files.push((format!("share/X11/xkb/{}", &path["usr/share/X11/xkb/".len()..]), data.clone(), mode));
        }
    }

    // Своя glibc (trixie) и её загрузчик под коротким именем.
    let mut glibc: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &GLIBC, &mut glibc)?;
    for name in GLIBC_LIBS {
        let Some((data, mode)) = follow(&glibc, &format!("usr/lib/x86_64-linux-gnu/{name}"), 0) else {
            bail!("в libc6 trixie нет {name}");
        };
        files.push((format!("glibc/{name}"), data.clone(), mode));
        if name == "ld-linux-x86-64.so.2" {
            files.push((String::from("glibc/ld.so"), data.clone(), 0o755));
        }
    }

    for (rest, (data, mode)) in &taken {
        // Библиотеки из архива кладутся только по `DT_NEEDED` — ниже.
        if rest.starts_with(BUNDLED) {
            continue;
        }
        // Программы — на свою glibc.
        if ["bin/wine", "bin/wineserver", "lib/wine/x86_64-unix/wine", "lib/wine/x86_64-unix/wine64"].contains(&rest.as_str()) {
            let mut data = data.clone();
            if !patch_interp(&mut data) {
                bail!("{rest}: нет загрузчика {:?}", String::from_utf8_lossy(INTERP_OLD));
            }
            todo.extend(
                needed(&data).with_context(|| format!("{rest}: не разобран ELF"))?.into_iter().map(|name| (name, rest.clone())),
            );
            files.push((rest.clone(), data, *mode));
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
        // Свои модули Wine и libasound из Debian 13 (кладёт `mesa::add`).
        if name == "ntdll.so" || name == "win32u.so" || name == "libasound.so.2" {
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

    // OpenGL и Vulkan на процессоре (шаги 5-6, 12.11).
    crate::mesa::add(&cache, &mut files, patch_interp)?;

    let script = concat!("#!/bin/bash\n\
        # GE-Proton (12.11.2026): программы и игры Windows без Steam, окна —\n\
        # через сервер Wayland ядра, синхронизация — fsync (futex_waitv).\n\
        P=/opt/proton\n\
        # Своя glibc (2.41) первой: модули GE собраны под 2.38+.\n\
        export LD_LIBRARY_PATH=$P/glibc:$P/lib/x86_64-linux-gnu:$P/lib\n\
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
        # Чисто 64-битный префикс (как `proton` по умолчанию): иначе Wine\n\
        # ищет для `wineboot` загрузчик в `i386-unix`, которого в пакете нет.\n\
        export WINEARCH=${WINEARCH:-win64}\n\
        # fsync — да (futex_waitv есть с 12.11), ntsync — нет (/dev/ntsync нет),\n\
        # esync — запасной (eventfd).\n\
        export WINEFSYNC=${WINEFSYNC:-1}\n\
        export WINEESYNC=${WINEESYNC:-1}\n\
        export WINENTSYNC=0\n\
        # `tabtip.exe` (экранная клавиатура, её запускает explorer) - выключен\n\
        # (13.11): с работающим RPC он вешал UI Automation (`com_event_handlers_cs`),\n\
        # и создание окна игры ждало его без конца. `nsiproxy.sys` снова включён\n\
        # (16.11): ядро отвечает netlink, SIOCGIF* и /proc/net.\n\
        export WINEDLLOVERRIDES=\"mscoree,mshtml,tabtip.exe=${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}\"\n\
        export WINEDEBUG=${WINEDEBUG:-fixme-all}\n\
        # `/tmp` у FossAi переживает перезагрузку: сокет wineserver прошлой\n\
        # загрузки остаётся обычным файлом, и Wine падает «is not a socket».\n\
        for s in /tmp/.wine-*/server-*/socket; do [ -e \"$s\" ] && [ ! -S \"$s\" ] && LD_LIBRARY_PATH= rm -f \"$s\"; done\n\
        # OpenGL окон (wined3d) через EGL winewayland: в GE по умолчанию выключен.\n\
        export WINE_USE_EGL=${WINE_USE_EGL:-1}\n\
        # Звук: libasound со своими настройками (ALSA ядра, `plug` поверх hw:0,0).\n\
        export ALSA_CONFIG_DIR=$P/share/alsa\n\
        export ALSA_CONFIG_PATH=$P/share/alsa/alsa.conf:$P/share/alsa/fossai.conf\n\
        export XKB_CONFIG_ROOT=$P/share/X11/xkb\n\
        # OpenGL и Vulkan на процессоре (Mesa trixie, 12.11): EGL через glvnd,\n\
        # llvmpipe и lavapipe.\n\
        export __EGL_VENDOR_LIBRARY_FILENAMES=$P/share/glvnd/egl_vendor.d/50_mesa.json\n\
        export LIBGL_DRIVERS_PATH=$P/lib/dri\n\
        export LIBGL_ALWAYS_SOFTWARE=1\n\
        export GALLIUM_DRIVER=llvmpipe\n\
        # NVK (18.11.2026): есть узел nouveau ядра /dev/dri/renderD128 - оба\n\
        # драйвера, NVK первым (DXVK берёт дискретную видеокарту, lavapipe -\n\
        # запасной); FOSSAI_VULKAN=lvp - только lavapipe, =nvk - только NVK, =radv - только RADV (AMD, узел amdgpu).\n\
        V=$P/share/vulkan/icd.d\n\
        case \"${FOSSAI_VULKAN:-auto}\" in\n\
        \tlvp) export VK_DRIVER_FILES=$V/lvp_icd.x86_64.json ;;\n\
        \tnvk) export VK_DRIVER_FILES=$V/nouveau_icd.x86_64.json ;;\n\
        \tradv) export VK_DRIVER_FILES=$V/radeon_icd.x86_64.json ;;\n\
        \t*) if [ -e /dev/dri/renderD128 ] && [ -e $V/radeon_icd.x86_64.json ] && grep -qs 'DRIVER=amdgpu' /sys/class/drm/renderD128/device/uevent; then\n\
        \t\texport VK_DRIVER_FILES=$V/radeon_icd.x86_64.json:$V/lvp_icd.x86_64.json\n\
        \telif [ -e /dev/dri/renderD128 ] && [ -e $V/nouveau_icd.x86_64.json ]; then\n\
        \t\texport VK_DRIVER_FILES=$V/nouveau_icd.x86_64.json:$V/lvp_icd.x86_64.json\n\
        \telse\n\
        \t\texport VK_DRIVER_FILES=$V/lvp_icd.x86_64.json\n\
        \tfi ;;\n\
        esac\n\
        export VK_ICD_FILENAMES=$VK_DRIVER_FILES\n\
        # `--env=ИМЯ=ЗНАЧЕНИЕ` (сколько угодно, первыми) — своя переменная: у\n\
        # `run` оболочки окружения нет (WAYLAND_DEBUG=client и подобные).\n\
        while case \"$1\" in --env=*) true ;; *) false ;; esac; do export \"${1#--env=}\"; shift; done\n\
        case \"$1\" in --debug=*) export WINEDEBUG=\"${1#--debug=}\"; shift ;; esac\n", r#"
# Кеши шейдеров (P3, 18.11): первый запуск игры - чёрное окно, пока LLVM
# лавапайпа/llvmpipe компилирует шейдеры. Mesa 25.0.7 кладёт готовый машинный
# код llvmpipe и lavapipe в дисковый кеш (`get_disk_shader_cache`, база
# `mesa_shader_cache_db`), DXVK GE-Proton11 - свой кеш шейдеров
# (`DXVK_SHADER_CACHE_PATH`; кеша состояний `DXVK_STATE_CACHE_PATH` в нём
# уже нет). Всё - в домашнем каталоге, один на все префиксы: на установленной
# системе второй запуск берёт готовое; на живой флешке дом в памяти, и кеш
# живёт до перезагрузки. Журнал DXVK - туда же, а не `ИГРА_d3d9.log` в
# каталог игры (строки журнала по-прежнему идут и в stderr).
C=$HOME/.cache/fossai-proton
export MESA_SHADER_CACHE_DIR=${MESA_SHADER_CACHE_DIR:-$C/mesa}
export MESA_SHADER_CACHE_MAX_SIZE=${MESA_SHADER_CACHE_MAX_SIZE:-1G}
export DXVK_SHADER_CACHE_PATH=${DXVK_SHADER_CACHE_PATH:-$C/dxvk}
export DXVK_STATE_CACHE_PATH=${DXVK_STATE_CACHE_PATH:-$C/dxvk}
export DXVK_LOG_PATH=${DXVK_LOG_PATH:-$C/log}
LD_LIBRARY_PATH= mkdir -p "$MESA_SHADER_CACHE_DIR" "$DXVK_SHADER_CACHE_PATH" "$DXVK_LOG_PATH"
# Потоки рисования llvmpipe/lavapipe (P3/P9): без переменной Mesa берёт все
# ЦП (16 на Ryzen 8 ядер/16 потоков), и каждый кадр будит 16 потоков через
# futex. По умолчанию - физические ядра: `cpu cores` из /proc/cpuinfo, если
# `siblings` больше (SMT, как у Linux); ядро FossAi пишет `cpu cores` = все
# ЦП и без `siblings` - тогда половина при 8 ЦП и больше, иначе все.
# Своё значение - `--env=LP_NUM_THREADS=N`.
if [ -z "$LP_NUM_THREADS" ]; then
    n=0; c=0; s=0
    while IFS=: read -r k v; do
        v=${v//[[:space:]]/}
        case $k in processor*) n=$((n + 1)) ;; 'cpu cores'*) c=${v:-0} ;; siblings*) s=${v:-0} ;; esac
    done < /proc/cpuinfo
    if [ "$s" -gt "$c" ] && [ "$c" -gt 0 ]; then t=$c; elif [ $n -ge 8 ]; then t=$((n / 2)); else t=$n; fi
    [ "$t" -gt 0 ] && export LP_NUM_THREADS=$t
fi
# `--desktop=ШxВ` (P2, 18.11) - программа в виртуальном рабочем столе Wine
# этого размера (`explorer /desktop=`): игра видит монитор ШxВ и рисует
# меньше точек (720p - вчетверо меньше 1440p), окно стола - одно. ОПЫТ: в
# QEMU (18.11) winewayland показал стол отдельным окном 665x434 и окно
# программы - своим, d3dprobe через DXVK кадров не дал (Wine ещё просит
# `zwp_pointer_constraints_v1` для ClipCursor стола, P10).
# `--bench=СЕКУНДЫ[,ПОДПИСЬ]` (P1) - замер: строки кадров Wine (`+fps`:
# wined3d и Vulkan/DXVK через win32u) или d3dprobe собираются, и через
# СЕКУНДЫ в журнал уходит `bench: ПОДПИСЬ avg N fps ...`, программа снимается.
D=; B=; R=wined3d
while :; do
    case "$1" in
        --desktop=*) D=${1#--desktop=}; shift ;;
        --bench=*) B=${1#--bench=}; shift ;;
        *) break ;;
    esac
done
# Путь от стола (P12): ядро делит строку запуска по пробелам и кавычек не
# знает, поэтому путь приходит с `%20` (и `%25` вместо `%`).
unpct() { local e=${1//\%/\\x}; printf -v U '%b' "$e"; }
# `--open=ФАЙЛ` - двойной щелчок по .exe/.msi/.lnk/.bat в «Файлах»: в общем
# префиксе (`$WINEPREFIX`, по умолчанию ~/.proton), из каталога файла.
# `--shortcut=ФАЙЛ.desktop` - пункт «Пуска» из ярлыка, который
# winemenubuilder установщика записал в `ПРЕФИКС/drive_c/proton_shortcuts/`:
# префикс - по пути ярлыка, команда - `start /unix ЯРЛЫК.lnk` из `Exec=`.
case "$1" in
    --open=*)
        unpct "${1#--open=}"; f=$U; shift
        cd "${f%/*}" 2>/dev/null
        case "${f,,}" in
            *.msi) set -- msiexec /i "$f" "$@" ;;
            *.lnk|*.bat|*.cmd) set -- start /wait /unix "$f" "$@" ;;
            *) set -- "$f" "$@" ;;
        esac
        echo "fossai-proton: open $f" ;;
    --shortcut=*)
        unpct "${1#--shortcut=}"; f=$U; shift
        export WINEPREFIX=${f%/drive_c/proton_shortcuts/*}
        x=; w=
        while IFS= read -r l; do
            case $l in Exec=*) x=${l#Exec=} ;; Path=*) w=${l#Path=} ;; esac
        done < "$f"
        # `Exec=env WINEPREFIX="…" wine …start.exe /Unix ПУТЬ`: winemenubuilder
        # экранирует пробел и прочее как `\\ `, обратную косую - как `\\\\`.
        case $x in *' /Unix '*|*' /unix '*) ;; *) echo "fossai-proton: no /Unix in $f: $x"; exit 1 ;; esac
        x=${x#* /[Uu]nix }; u=; i=0
        while [ $i -lt ${#x} ]; do
            if [ "${x:i:4}" = '\\\\' ]; then u+='\'; i=$((i + 4))
            elif [ "${x:i:2}" = '\\' ]; then u+=${x:i+2:1}; i=$((i + 3))
            elif [ "${x:i:1}" = '\' ]; then u+=${x:i+1:1}; i=$((i + 2))
            elif [ "${x:i:1}" = ' ' ]; then break
            else u+=${x:i:1}; i=$((i + 1)); fi
        done
        [ -n "$w" ] && cd "$w" 2>/dev/null
        echo "fossai-proton: shortcut $f -> $u (prefix $WINEPREFIX)"
        set -- start /wait /unix "$u" "$@" ;;
esac
"#, "\
        # `--run=ПРОГРАММА` - проба из bin/ пакета (es2gears_wayland, vkcube) с\n\
        # тем же окружением.\n\
        case \"$1\" in --run=*) n=\"${1#--run=}\"; shift; exec \"$P/bin/$n\" \"$@\" ;; esac\n\
        # `--fresh` - начать с нового префикса (у `rm` оболочки FossAi нет `-r`).\n\
        case \"$1\" in --fresh) shift; LD_LIBRARY_PATH= rm -rf \"$WINEPREFIX\" ;; esac\n\
        case \"$1\" in --server-debug=*) LD_LIBRARY_PATH= mkdir -p \"$WINEPREFIX\"; $P/bin/wineserver -d\"${1#--server-debug=}\"; shift ;; esac\n\
        # `--dxvk` - Direct3D 8-11 через DXVK (Vulkan), как в Proton: DLL DXVK\n\
        # кладутся в префикс поверх wined3d и включаются как native.\n\
        # Новый префикс — заранее и до конца (13.11): в QEMU он строится дольше\n\
        # ожидания Wine («boot event wait timed out» → программа без kernel32).\n\
        # С 17.11 — из готового шаблона пакета (`share/fossai/prefix/`, снят\n\
        # стендом `proton-prefix`; один на win64 и wow64): секунды вместо минут\n\
        # `wineboot -i`; без шаблона — по-старому. Пустые каталоги и ссылки\n\
        # (`dosdevices/c:`) — списком `prefix.list`: в пакете их не бывает.\n\
        case \"$1\" in --server-debug=*) ;; *)\n\
            if [ ! -f \"$WINEPREFIX/system.reg\" ]; then\n\
                T=$P/share/fossai/prefix\n\
                if [ -f \"$T.list\" ]; then\n\
                    LD_LIBRARY_PATH= mkdir -p \"$WINEPREFIX\"\n\
                    LD_LIBRARY_PATH= cp -r \"$T/.\" \"$WINEPREFIX/\"\n\
                    d=(); l=()\n\
                    while IFS=$'\\t' read -r k a b; do\n\
                        case $k in d) d+=(\"$WINEPREFIX/$a\") ;; l) l+=(\"$a\" \"$b\") ;; esac\n\
                    done < \"$T.list\"\n\
                    [ ${#d[@]} -gt 0 ] && LD_LIBRARY_PATH= mkdir -p \"${d[@]}\"\n\
                    for ((i = 0; i < ${#l[@]}; i += 2)); do\n\
                        LD_LIBRARY_PATH= ln -sfn \"${l[i+1]}\" \"$WINEPREFIX/${l[i]}\"\n\
                    done\n\
                    # `.update-timestamp` — из шаблона как есть: его пишет сам Wine\n\
                    # (время `wine.inf`, у файлов пакета 0). Своё значение от `stat`\n\
                    # Debian с ним не совпало, и первый `wineboot` переделывал\n\
                    # префикс заново (+3,5 мин в QEMU, 17.11).\n\
                    echo \"fossai-proton: prefix from template\"\n\
                else\n\
                    $P/lib/wine/x86_64-unix/wine wineboot -i\n\
                    $P/bin/wineserver -w\n\
                fi\n\
            fi ;; esac\n\
        # `--prefix` - только приготовить префикс (`bin/tmnf`).\n\
        case \"$1\" in --prefix) exit 0 ;; esac\n\
        case \"$1\" in --dxvk) shift; R=dxvk\n\
            s=\"$WINEPREFIX/drive_c/windows\"\n\
            # `cp` Debian - со своей glibc, без нашего LD_LIBRARY_PATH (2.41).\n\
            LD_LIBRARY_PATH= cp -f $P/lib/wine/dxvk/x86_64-windows/*.dll \"$s/system32/\"\n\
            [ -d \"$s/syswow64\" ] && LD_LIBRARY_PATH= cp -f $P/lib/wine/dxvk/i386-windows/*.dll \"$s/syswow64/\"\n\
            export WINEDLLOVERRIDES=\"d3d8,d3d9,d3d10core,d3d11,dxgi=n;$WINEDLLOVERRIDES\" ;; esac\n\
        [ $# -eq 0 ] && set -- notepad\n\
        # Загрузчик архитектуры напрямую: `bin/wine` GE ищет `ntdll.so` в\n\
        # `lib/wine/`, а не в `lib/wine/x86_64-unix/` (12.11).\n\
        W=$P/lib/wine/x86_64-unix/wine\n", r#"
if [ -n "$D" ]; then
    echo "fossai-proton: virtual desktop $D"
    set -- explorer "/desktop=FossAi,$D" "$@"
fi
# Кадры в журнал ядра (20.11): с `+fps` в WINEDEBUG (`tmnf` ставит его сам)
# строки Wine `trace:fps:… approx N fps` раз в 5 с сводятся в одну
# `fps: ПРОГРАММА СРЕДНЕЕ (min, max)` в `/dev/kmsg` - fps игры видно в журнале
# на флешке, даже при запуске со стола. Остальной вывод - в stderr как был.
if [ -z "$B" ]; then
    case ,$WINEDEBUG, in *,+fps,*) [ -e /dev/kmsg ] || exec $W "$@" ;; *) exec $W "$@" ;; esac
    p=${1##*/}; p=${p##*\\}; [ "$p" = explorer ] && { p=${3##*/}; p=${p##*\\}; }
    $W "$@" 2>&1 | {
        n=0; sum=0; lo=; hi=0; t=$SECONDS
        while IFS= read -r line; do
            case $line in
                *'trace:fps:'*'approx '*)
                    v=${line#*approx }; v=${v%%fps*}
                    case $v in ''|*[!0-9.]*|.*) continue ;; esac
                    i=${v%%.*}; f=${v#*.}; [ "$f" = "$v" ] && f=0; f=${f}00
                    c=$((10#$i * 100 + 10#${f:0:2}))
                    n=$((n + 1)); sum=$((sum + c))
                    if [ -z "$lo" ] || [ $c -lt $lo ]; then lo=$c; fi
                    [ $c -gt $hi ] && hi=$c ;;
                *) printf '%s\n' "$line" >&2 ;;
            esac
            if [ $n -gt 0 ] && [ $((SECONDS - t)) -ge 5 ]; then
                a=$((sum / n))
                printf 'fps: %s %d.%02d (min %d.%02d, max %d.%02d, %d samples in %d s)\n' "$p" $((a / 100)) $((a % 100)) \
                    $((lo / 100)) $((lo % 100)) $((hi / 100)) $((hi % 100)) $n $((SECONDS - t)) > /dev/kmsg
                n=0; sum=0; lo=; hi=0; t=$SECONDS
            fi
        done
    }
    exit ${PIPESTATUS[0]}
fi
# Замер (`--bench`): вывод Wine идёт через этот цикл и дальше в stderr как
# был; значения кадров - сотыми, арифметика bash целая. Источник один:
# первый встреченный (`trace:fps:ФУНКЦИЯ`), но d3dprobe важнее строк Wine.
secs=${B%%,*}; label=${B#*,}
[ "$label" = "$B" ] && { p=${1##*/}; [ "$p" = explorer ] && p=${3##*/}; label="${p##*\\} $R ${D:-window}"; }
case ,$WINEDEBUG, in *,+fps,*) ;; *) export WINEDEBUG="${WINEDEBUG:+$WINEDEBUG,}+fps" ;; esac
# Строки `bench:` - и в журнал ядра (20.11): со стола у программы нет
# терминала, а в терминале строку рвут коды курсора.
say() { echo "$1"; [ -e /dev/kmsg ] && echo "$1" > /dev/kmsg; }
say "bench: $label for $secs s, LP_NUM_THREADS=$LP_NUM_THREADS"
t0=${EPOCHREALTIME/[.,]/}
$W "$@" 2>&1 | {
    # СЕКУНДЫ считаются от первых кадров (запуск Wine в QEMU - десятки
    # секунд); не дождались кадров за 10 минут - конец.
    # Первое значение (прогрев, компиляция шейдеров) в среднее не идёт.
    n=0; sum=0; lo=; hi=0; first=; src=; skip=1; end=$((SECONDS + 600))
    while :; do
        if IFS= read -r -t 5 line; then
            printf '%s\n' "$line" >&2
            v=; s=
            case $line in
                *'d3dprobe: '*' FPS'*) v=${line##*= }; v=${v%% FPS*}; s=d3dprobe ;;
                *'trace:fps:'*'approx '*) v=${line#*approx }; v=${v%%fps*}; s=${line#*trace:fps:}; s=${s%% *} ;;
            esac
            case $v in ''|*[!0-9.]*|.*) v= ;; esac
            if [ -n "$v" ]; then
                if [ -z "$first" ]; then first=$(( (${EPOCHREALTIME/[.,]/} - t0) / 100000 )); end=$((SECONDS + secs)); fi
                if [ "$s" = d3dprobe ] && [ "$src" != d3dprobe ]; then src=; n=0; sum=0; lo=; hi=0; skip=1; fi
                [ -z "$src" ] && src=$s
                if [ "$s" = "$src" ] && [ -n "$skip" ]; then
                    skip=
                elif [ "$s" = "$src" ]; then
                    i=${v%%.*}; f=${v#*.}; [ "$f" = "$v" ] && f=0; f=${f}00
                    c=$((10#$i * 100 + 10#${f:0:2}))
                    n=$((n + 1)); sum=$((sum + c))
                    if [ -z "$lo" ] || [ $c -lt $lo ]; then lo=$c; fi
                    [ $c -gt $hi ] && hi=$c
                fi
            fi
        elif [ $? -le 128 ]; then
            eof=1
            break
        fi
        [ $SECONDS -ge $end ] && break
    done
    if [ $n -gt 0 ]; then
        a=$((sum / n))
        printf -v a '%d.%02d' $((a / 100)) $((a % 100))
        printf -v lo '%d.%02d' $((lo / 100)) $((lo % 100))
        printf -v hi '%d.%02d' $((hi / 100)) $((hi % 100))
        say "bench: $label avg $a fps (min $lo, max $hi, $n samples from $src), first frames after $((first / 10)).$((first % 10)) s, LP_NUM_THREADS=$LP_NUM_THREADS"
    else
        say "bench: $label no frames in $secs s"
    fi
    # Программа, которая выходит сама (d3dprobe со своим сроком), - дождаться:
    # после `wineserver -k` следующий запуск в том же префиксе получал
    # «Invalid window handle» (замер 18.11, живая флешка в QEMU). Не вышла за
    # 60 с (игра) - снять префикс и подождать сервер, не дольше 30 с (снятие
    # задач ядром может застрять, P4).
    grace=$((SECONDS + 60))
    while [ -z "$eof" ] && [ $SECONDS -lt $grace ]; do
        if IFS= read -r -t 5 line; then printf '%s\n' "$line" >&2; elif [ $? -le 128 ]; then eof=1; fi
    done
    if [ -z "$eof" ]; then
        say "bench: $label still running after the measurement, wineserver -k"
        $P/bin/wineserver -k 2>/dev/null
        $P/bin/wineserver -w 2>/dev/null & w=$!
        for i in {1..30}; do kill -0 $w 2>/dev/null || break; LD_LIBRARY_PATH= sleep 1; done
        [ $i -ge 30 ] && echo "bench: wineserver still alive after 30 s"
    fi
}
"#);
    files.push((String::from("bin/fossai-proton"), script.as_bytes().to_vec(), 0o755));
    files.push((String::from("bin/proton-bench"), BENCH.as_bytes().to_vec(), 0o755));
    files.push((String::from("bin/fossai-gametest"), GAMETEST.as_bytes().to_vec(), 0o755));
    // Новый 64-битный процесс (`wineboot`, программа из программы) ntdll GE
    // запускает загрузчиком `i386-unix/wine64`; 32-битной части Linux у нас
    // нет — переходник отдаёт запуск 64-битному загрузчику. Интерпретатор —
    // программа без libc (`libc/linux/wine64-stub.c`): сценарий через `bash`
    // Debian не годится — его загрузчик с glibc 2.41 из `LD_LIBRARY_PATH` не
    // уживается, а `ld.so` 2.41, запущенная напрямую, падает (12.11).
    let wine64 = cbuild::build_linux_static("wine64-stub")?;
    files.push((String::from("lib/wine/i386-unix/wine64"), wine64, 0o755));
    files.push((String::from("share/wine/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    // Проба DirectX 9/11 (`tools/d3dprobe`, программа Windows) - для сценариев
    // wined3d и DXVK (12.11).
    files.push((String::from("share/fossai/d3dprobe.exe"), d3dprobe()?, 0o755));
    // Шаблоны префикса (17.11): их снимает сценарий `proton-prefix` (настоящий
    // `wineboot` в QEMU) в `build/proton-prefix/`. Нет снимка — пакет без
    // шаблона, префикс создаёт `wineboot` при первом запуске (минуты).
    if !add_template(PREFIX_SNAPSHOT, &mut files)? {
        say!("proton: нет шаблона префикса — его создаст wineboot (сценарий proton-prefix)");
    }

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

/// `bin/fossai-gametest [vk|d3d|tmnf|all]` (20.11.2026): пользовательская
/// часть `gpu gametest` — всё на NVK (видеокарта через GSP-RM), по ступени на
/// строку `gametest: PASS|FAIL имя за N мс`, в конце `gametest: summary`.
/// Ядро запускает её с выводом в канал и переписывает строки в свой журнал
/// (на флешку). Руками: `run -b /opt/debian/bin/bash /opt/proton/bin/fossai-gametest`.
const GAMETEST: &str = r#"#!/bin/bash
exec 2>&1
P=/opt/proton/bin/fossai-proton
B=/opt/debian/bin/bash
X=/opt/proton/share/fossai/d3dprobe.exe
# Только NVK (fossai-proton читает FOSSAI_VULKAN до разбора `--env=`).
export FOSSAI_VULKAN=nvk
NVK=
WHAT=${1:-all}
VKFAIL=
pass=0; fail=0; names=
now() { echo $(( ${EPOCHREALTIME/[.,]/} / 1000 )); }
say() { echo "gametest: $*"; }
# Предел времени: `timeout` coreutils Debian, если есть.
T=; [ -x /opt/debian/bin/timeout ] && T="/opt/debian/bin/timeout -k 10"
# step ИМЯ СЕКУНДЫ КОМАНДА... - вывод команды идёт как есть, затем PASS/FAIL.
step() {
    local name=$1 secs=$2; shift 2
    local t=$(now) rc
    say "run $name (limit ${secs} s)"
    if [ -n "$T" ]; then $T $secs "$@"; else "$@"; fi
    rc=$?
    local ms=$(( $(now) - t ))
    if [ $rc = 0 ]; then
        pass=$((pass + 1)); say "PASS $name in $ms ms"
    else
        fail=$((fail + 1)); names="$names $name"
        local why=; [ $rc = 124 ] && why=", time limit"
        say "FAIL $name (exit $rc$why) after $ms ms"
    fi
    return $rc
}
say "start: $(date '+%F %T'), what=$WHAT, node $( [ -e /dev/dri/renderD128 ] && echo present || echo ABSENT)"
if [ ! -e /dev/dri/renderD128 ]; then
    say "FAIL no /dev/dri/renderD128 - run 'nvk hw on' (or 'gpu gametest') first"
    say "summary: 0 passed, 1 failed (node)"
    exit 1
fi
case $WHAT in all|vk)
    # 1. Vulkan на NVK: устройство, отправка с забором, заливка и копия 1 МиБ.
    step vkprobe 300 $B $P $NVK --run=vkprobe NVK || VKFAIL=1
    # 2. Что видит загрузчик (имя, драйвер, версии) - для журнала.
    step vulkaninfo 120 $B $P $NVK --run=vulkaninfo --summary
    # 3. vkcube: 600 кадров через zwp_linux_dmabuf_v1 (окно на столе).
    t=$(now)
    if step vkcube 180 $B $P $NVK --run=vkcube --wsi wayland --c 600; then
        ms=$(( $(now) - t )); [ $ms -gt 0 ] && say "vkcube: 600 frames in $ms ms = $(( 600000 / ms )) fps (with start-up)"
    fi ;;
esac
# Без Vulkan на NVK DXVK и игре на видеокарте не на чем работать.
if [ -n "$VKFAIL" ] && [ "$WHAT" = all ]; then
    say "vkprobe failed - skipping d3d9-dxvk-nvk and tmnf-nvk (the log above has the NVK ioctl trace)"
    WHAT=none
fi
case $WHAT in all|d3d)
    # 4. Direct3D 9 (как у TMNF) через DXVK на NVK: 20 с кадров, замер.
    step d3d9-dxvk-nvk 600 $B $P $NVK --bench=20,d3dprobe9-dxvk-nvk --dxvk $X 9 20 ;;
esac
case $WHAT in all|tmnf)
    # 5. TMNF через DXVK на NVK: замер 60 с (`bench:` - к/с), игра снимается.
    if [ -x /opt/tmnf/bin/tmnf ] || [ -f /opt/tmnf/bin/tmnf ]; then
        step tmnf-nvk 1800 $B /opt/tmnf/bin/tmnf --nvk --bench=60
    else
        say "tmnf: package not installed - skipped"
    fi ;;
esac
say "summary: $pass passed, $fail failed${names:+ ($names )}"
"#;

/// `bin/proton-bench [СЕКУНДЫ] [all|cache|size|threads]` (P1/P2/P3, 18.11):
/// матрица замеров одной командой — на стенде и на живой флешке Романа
/// (`run -b /opt/debian/bin/bash /opt/proton/bin/proton-bench`), итог — строки
/// `bench:` в журнале. d3dprobe (Direct3D 9, как у TMNF) через DXVK и wined3d:
/// холодный и тёплый кеш шейдеров, размеры кадра, потоки lavapipe.
const BENCH: &str = r#"#!/bin/bash
P=/opt/proton/bin/fossai-proton
X=/opt/proton/share/fossai/d3dprobe.exe
B=/opt/debian/bin/bash
export PATH=/opt/debian/bin:$PATH
S=${1:-20}; WHAT=${2:-all}
n=0; while IFS=: read -r k v; do case $k in processor*) n=$((n + 1)) ;; esac; done < /proc/cpuinfo
echo "bench: start, $S s per run, $n CPUs, what=$WHAT"
# подпись, --env=… или -, --dxvk или -, затем аргументы d3dprobe (API, размер)
one() {
    local label=$1 env=$2 mode=$3 api=$4 size=$5 a=()
    [ "$env" != - ] && a+=("--env=$env")
    a+=("--bench=$S,$label")
    case $mode in -) ;; *) a+=($mode) ;; esac
    $B $P "${a[@]}" $X $api $((S + 15)) $size
    sleep 3
}
C=$HOME/.cache/fossai-proton
mkdir -p "$C"
# Один `explorer` (окно стола Wine) на все замеры: его держит скрытое окно
# `d3dprobe hold`. Иначе каждый запуск поднимал его заново, а в QEMU он
# поднимается дольше, чем Wine его ждёт («The explorer process failed to
# start», 18.11), и в замер шёл бы ещё и запуск explorer.
H=$C/hold.log
rm -f "$H"
$B $P $X hold 14400 2> "$H" &
for i in {1..120}; do
    held=; [ -f "$H" ] && while IFS= read -r l; do case $l in *'holding the desktop'*) held=1 ;; esac; done < "$H"
    [ -n "$held" ] && break
    sleep 5
done
echo "bench: desktop holder ${held:+up}${held:-NOT up} after $((i * 5)) s"
if [ $WHAT = all ] || [ $WHAT = cache ]; then
    # Кеш шейдеров (P3): первый запуск - с пустым кешем, второй - с тёплым.
    rm -rf "$C/mesa" "$C/dxvk"
    one "d3d9 dxvk 640x480 cold-cache" - --dxvk 9 640x480
    one "d3d9 dxvk 640x480 warm-cache" - --dxvk 9 640x480
    echo "bench: cache $(du -sk "$C/mesa" 2>/dev/null | cut -f1) KiB mesa, $(du -sk "$C/dxvk" 2>/dev/null | cut -f1) KiB dxvk"
fi
if [ $WHAT = all ] || [ $WHAT = size ]; then
    # Размер кадра (P2): цена каждой точки на процессоре.
    for r in dxvk wined3d; do
        m=-; [ $r = dxvk ] && m=--dxvk
        for size in 640x480 1280x720 1920x1080 2560x1440; do
            one "d3d9 $r $size" - $m 9 $size
        done
    done
    # Виртуальный рабочий стол Wine (`--desktop=`), как у `tmnf --720p`.
    $B $P --desktop=1280x720 --bench=$S,"d3d9 dxvk desktop-1280x720" --dxvk $X 9 $((S + 15)) 1280x720
    sleep 3
fi
if [ $WHAT = all ] || [ $WHAT = threads ]; then
    # Потоки lavapipe (P9): по умолчанию - половина ЦП (см. fossai-proton).
    for t in 2 4 8 12 16; do
        [ $t -gt $n ] && break
        one "d3d9 dxvk 1280x720 LP_NUM_THREADS=$t" LP_NUM_THREADS=$t --dxvk 9 1280x720
    done
fi
$B $P --run=wineserver -k
echo "bench: done"
"#;

/// Убрать `.` и `..` из относительного пути.
/// Собирает `tools/d3dprobe` (Rust, x86_64-pc-windows-msvc, статический CRT)
/// и отдаёт `d3dprobe.exe`.
fn d3dprobe() -> Result<Vec<u8>> {
    let dir = crate::paths::workspace_root().join("tools/d3dprobe");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(cargo)
        .args(["build", "--release", "--target", "x86_64-pc-windows-msvc"])
        .current_dir(&dir)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()?;
    anyhow::ensure!(status.success(), "d3dprobe не собрался");
    Ok(fs::read(dir.join("target/x86_64-pc-windows-msvc/release/d3dprobe.exe"))?)
}

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
