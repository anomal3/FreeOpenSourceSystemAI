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

    // OpenGL и Vulkan на процессоре (шаги 5-6, 12.11).
    crate::mesa::add(&cache, &mut files, patch_interp)?;

    let script = "#!/bin/bash\n\
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
        export WINEDLLOVERRIDES=\"mscoree,mshtml=${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}\"\n\
        export WINEDEBUG=${WINEDEBUG:-fixme-all}\n\
        export XKB_CONFIG_ROOT=$P/share/X11/xkb\n\
        # OpenGL и Vulkan на процессоре (Mesa trixie, 12.11): EGL через glvnd,\n\
        # llvmpipe и lavapipe.\n\
        export __EGL_VENDOR_LIBRARY_FILENAMES=$P/share/glvnd/egl_vendor.d/50_mesa.json\n\
        export LIBGL_DRIVERS_PATH=$P/lib/dri\n\
        export LIBGL_ALWAYS_SOFTWARE=1\n\
        export GALLIUM_DRIVER=llvmpipe\n\
        export VK_DRIVER_FILES=$P/share/vulkan/icd.d/lvp_icd.x86_64.json\n\
        export VK_ICD_FILENAMES=$VK_DRIVER_FILES\n\
        # `--env=ИМЯ=ЗНАЧЕНИЕ` (сколько угодно, первыми) — своя переменная: у\n\
        # `run` оболочки окружения нет (WAYLAND_DEBUG=client и подобные).\n\
        while case \"$1\" in --env=*) true ;; *) false ;; esac; do export \"${1#--env=}\"; shift; done\n\
        case \"$1\" in --debug=*) export WINEDEBUG=\"${1#--debug=}\"; shift ;; esac\n\
        # `--run=ПРОГРАММА` - проба из bin/ пакета (es2gears_wayland, vkcube) с\n\
        # тем же окружением.\n\
        case \"$1\" in --run=*) n=\"${1#--run=}\"; shift; exec \"$P/bin/$n\" \"$@\" ;; esac\n\
        # `--fresh` - начать с нового префикса (у `rm` оболочки FossAi нет `-r`).\n\
        case \"$1\" in --fresh) shift; LD_LIBRARY_PATH= rm -rf \"$WINEPREFIX\" ;; esac\n\
        case \"$1\" in --server-debug=*) LD_LIBRARY_PATH= mkdir -p \"$WINEPREFIX\"; $P/bin/wineserver -d\"${1#--server-debug=}\"; shift ;; esac\n\
        # `--dxvk` - Direct3D 8-11 через DXVK (Vulkan), как в Proton: DLL DXVK\n\
        # кладутся в префикс поверх wined3d и включаются как native.\n\
        case \"$1\" in --dxvk) shift\n\
            s=\"$WINEPREFIX/drive_c/windows\"\n\
            [ -d \"$s/system32\" ] || $P/lib/wine/x86_64-unix/wine wineboot -i\n\
            # `cp` Debian - со своей glibc, без нашего LD_LIBRARY_PATH (2.41).\n\
            LD_LIBRARY_PATH= cp -f $P/lib/wine/dxvk/x86_64-windows/*.dll \"$s/system32/\"\n\
            [ -d \"$s/syswow64\" ] && LD_LIBRARY_PATH= cp -f $P/lib/wine/dxvk/i386-windows/*.dll \"$s/syswow64/\"\n\
            export WINEDLLOVERRIDES=\"d3d8,d3d9,d3d10core,d3d11,dxgi=n;$WINEDLLOVERRIDES\" ;; esac\n\
        [ $# -eq 0 ] && set -- notepad\n\
        # Загрузчик архитектуры напрямую: `bin/wine` GE ищет `ntdll.so` в\n\
        # `lib/wine/`, а не в `lib/wine/x86_64-unix/` (12.11).\n\
        exec $P/lib/wine/x86_64-unix/wine \"$@\"\n";
    files.push((String::from("bin/fossai-proton"), script.as_bytes().to_vec(), 0o755));
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
