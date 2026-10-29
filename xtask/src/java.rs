//! Java для программ Linux: JetBrains Runtime 25 из дистрибутива Rider для
//! Linux — пакетом `jbr-25.fpk` (`/opt/java` после `pkg install java`). Первый
//! шаг к Rider: его интерфейс — программа на Java, и без неё дальше идти некуда.
//!
//! # Откуда Java
//!
//! Не из Debian, а из архива Rider (`jbr/`): это та самая Java, на которой
//! Rider работает, — сборка JetBrains с AWT поверх Wayland (`WLToolkit`,
//! `libawt_wlawt.so`). У OpenJDK из Debian 12 такого нет вовсе, а X11 в системе
//! нет. Архив качается целиком (2,4 ГБ, хеш — с сайта JetBrains) и ляжет в
//! основу пакета Rider; из него сейчас берётся только `jbr/`, потоком, без
//! распаковки шести с половиной гигабайт в память.
//!
//! # Что ещё в пакете
//!
//! Библиотеки, которые грузит AWT на Wayland (`libawt_wlawt.so`), и то, что
//! грузят они, — цепочка `DT_NEEDED` по пакетам Debian из [`PACKAGES`], минус
//! уже лежащее в корне `debian-12.fpk`. `fontconfig` AWT открывает сам
//! (`dlopen`), поэтому он в начале цепочки явно. Шрифты DejaVu и свой
//! `fonts.conf` — как у Chromium.
//!
//! # JIT
//!
//! Java-машина HotSpot на Linux x86-64 просит память «на запись и исполнение»
//! с первой секунды — под интерпретатор, который она генерирует при запуске,
//! так что без неё не запускается даже `java -version`. Пакет поэтому просит
//! право `jit`: ядро даёт такую память памятью кода (страница переключается
//! между записью и исполнением по обращению).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::debian::{Node, follow, unpack};
use crate::{cbuild, sdk};

/// Имя файла пакета: на носителе установщика имя сводится к 8.3 по куску до
/// последнего дефиса — `jbr`.
pub const PACKAGE_FILE: &str = "jbr-25.fpk";

/// Где лежит собранный пакет.
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Дистрибутив Rider для Linux x86-64 и его SHA-256 (из
/// `JetBrains.Rider-2026.2.3.1.tar.gz.sha256` на `download.jetbrains.com`).
pub const RIDER_URL: &str = "https://download.jetbrains.com/rider/JetBrains.Rider-2026.2.3.1.tar.gz";
pub const RIDER_SHA256: &str = "fa4b09a5f7cf4b6635b093adc7313991778a8dc74f25614a2b8976b04dee5d4e";
/// Каталог верхнего уровня в архиве — с пробелом в имени.
pub const RIDER_TOP: &str = "JetBrains Rider-2026.2.3.1/";

/// Где ищет `ld.so` (корень Debian 12, без usrmerge).
const LIB_DIRS: [&str; 4] = ["lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib", "usr/lib"];

/// С чего начинается цепочка библиотек: что грузит `libawt_wlawt.so` и что
/// AWT открывает сам. X11 (`libawt_xawt.so`) и звук (`libjsound.so`) не нужны:
/// окна идут через Wayland, а звука Java-программам пока не обещано.
const ROOTS: [&str; 6] = [
    "libwayland-client.so.0",
    "libwayland-cursor.so.0",
    "libxkbcommon.so.0",
    "libfreetype.so.6",
    "libfontconfig.so.1",
    "libz.so.1",
];

/// Пакеты Debian: путь в архиве и SHA-256 из `Packages` bookworm (снимок
/// 20261020, тот же, что у `crate::debian` и `crate::chromium`).
const PACKAGES: [(&str, &str); 14] = [
    // Раскладки для `libxkbcommon`: без них клавиатура окон Wayland не
    // заводится, и AWT падает на `Failed to create an XKB context`.
    ("pool/main/x/xkeyboard-config/xkb-data_2.35.1-1_all.deb", "28a79c61b785f403da93af58c39aed13c022032935e3e54362c6707169cfe982"),
    ("pool/main/w/wayland/libwayland-client0_1.21.0-1_amd64.deb", "1f002d028b8b79eec9847c636d8886d10dfe8c884cc2bebe18086b1391c5a28d"),
    ("pool/main/w/wayland/libwayland-cursor0_1.21.0-1_amd64.deb", "0b684467ec795024fcaae99ea4384959cc4052b2da5adec38c9b76dfed9bcf41"),
    ("pool/main/libx/libxkbcommon/libxkbcommon0_1.5.0-1_amd64.deb", "e3fe045b9a33a101de1c5a912a4a10928db055c3f68930f47eccbb44d7c7d54e"),
    ("pool/main/f/freetype/libfreetype6_2.12.1+dfsg-5+deb12u4_amd64.deb", "8043e479f73f29992d652e3f9dfe8b17f9780c7ea6330afe379ec5f9f188ac44"),
    ("pool/main/libp/libpng1.6/libpng16-16_1.6.39-2+deb12u5_amd64.deb", "a56d64bfaa9da12aafb83347909e62e6fd5fd251e6b34c194065911a30359978"),
    ("pool/main/b/brotli/libbrotli1_1.0.9-2+b6_amd64.deb", "563b4caec1aa5e876bd3355b36e7a38e1484baf5a293b48d1e8bd22db786e4d7"),
    ("pool/main/f/fontconfig/libfontconfig1_2.14.1-4_amd64.deb", "16ee38d374e064f534116dc442b086ef26f9831f1c0af7e5fb4fe4512e700649"),
    ("pool/main/f/fontconfig/fontconfig-config_2.14.1-4_amd64.deb", "281c66e46b95f045a0282a6c7a03b33de0e9a08d016897a759aaf4a04adfddbe"),
    ("pool/main/e/expat/libexpat1_2.5.0-1+deb12u2_amd64.deb", "2255e62fc22a86d2c544b8a3f516da9aee19383ad5742722ab4ce7f66a30dbc8"),
    ("pool/main/libf/libffi/libffi8_3.4.4-1_amd64.deb", "6d9f6c25c30efccce6d4bceaa48ea86c329a3432abb360a141f76ac223a4c34a"),
    ("pool/main/z/zlib/zlib1g_1.2.13.dfsg-1_amd64.deb", "d7dd1d1411fedf27f5e27650a6eff20ef294077b568f4c8c5e51466dc7c08ce4"),
    ("pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-6_all.deb", "8892669e51aab4dc56682c8e39d8ddb7d70fad83c369344e1e240bf3ca22bb76"),
    ("pool/main/g/gcc-12/gcc-12-base_12.2.0-14+deb12u1_amd64.deb", "1896a2aacf4ad681ff5eacc24a5b0ca4d5d9c9b9c9e4b6de5197bc1e116ea619"),
];

/// Шрифты: каталог в пакетах Debian и `fonts.conf`, который смотрит в
/// `/opt/java/fonts`.
const FONT_DIR: &str = "usr/share/fonts/truetype/dejavu/";
const FONTS_CONF: &str = "<?xml version=\"1.0\"?>\n\
    <!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n\
    <fontconfig>\n\
    \t<dir>/opt/java/fonts</dir>\n\
    \t<cachedir>/tmp/fontconfig</cachedir>\n\
    \t<alias><family>sans-serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>serif</family><prefer><family>DejaVu Sans</family></prefer></alias>\n\
    \t<alias><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias>\n\
    </fontconfig>\n";

/// Подробный журнал AWT на Wayland (`sun.awt.wl`) в стандартный вывод
/// ошибок.
const WL_LOG: &str = "handlers=java.util.logging.ConsoleHandler\n\
    java.util.logging.ConsoleHandler.level=ALL\n\
    .level=WARNING\n\
    sun.awt.wl.level=ALL\n";

/// Раскладки XKB в пакете `xkb-data`.
const XKB_DIR: &str = "usr/share/X11/xkb/";

/// Что из раскладок берётся. Не всё: раскладку окну присылает сервер Wayland
/// ядра (`wl_keyboard.keymap`), а `libxkbcommon` нужен каталог, без которого
/// он не заводит контекст, и база на случай сборки раскладки по именам
/// (`evdev`, `us`, `ru`). Весь `xkb-data` — триста файлов, и манифест пакета с
/// ними не помещается в свой предел.
const XKB_KEEP: [&str; 26] = [
    "rules/evdev",
    "keycodes/evdev",
    "keycodes/aliases",
    "types/",
    "compat/",
    "symbols/pc",
    "symbols/us",
    "symbols/ru",
    "symbols/inet",
    "symbols/latin",
    "symbols/keypad",
    "symbols/altwin",
    "symbols/level3",
    "symbols/level5",
    "symbols/group",
    "symbols/capslock",
    "symbols/ctrl",
    "symbols/compose",
    "symbols/shift",
    "symbols/srvr_ctrl",
    "symbols/nbsp",
    "symbols/kpdl",
    "symbols/eurosign",
    "symbols/terminate",
    "symbols/lv3",
    "symbols/lv5",
];

/// Пробные программы: запускаются из исходника (`java Hello.java`) — в JBR
/// есть `jdk.compiler`, и отдельный `javac` не нужен.
const HELLO: &str = "public class Hello {\n\
    \tpublic static void main(String[] args) {\n\
    \t\tSystem.out.println(\"java: hello from \" + System.getProperty(\"java.vm.name\") + \" \" + Runtime.version());\n\
    \t\tlong sum = 0;\n\
    \t\tfor (int i = 0; i < 50_000_000; i++) sum += i % 7;\n\
    \t\tSystem.out.println(\"java: loop sum \" + sum);\n\
    \t}\n\
    }\n";

/// Окно Swing: надпись и кнопка; с `self-test` нажимает её сам и открывает
/// модальный диалог (как «Manage Licenses» у Rider) — его и окно закрывает
/// крестиком стола стенд. Через три минуты выходит само, если не закрыли.
const WINDOW: &str = "import javax.swing.*;\n\
    import java.awt.*;\n\
    public class Window {\n\
    \tpublic static void main(String[] args) throws Exception {\n\
    \t\tboolean self = args.length > 0 && args[0].equals(\"self-test\");\n\
    \t\tSystem.out.println(\"java: toolkit \" + Toolkit.getDefaultToolkit().getClass().getName());\n\
    \t\tSwingUtilities.invokeAndWait(() -> {\n\
    \t\t\tJFrame frame = new JFrame(\"Java on FossAi\");\n\
    \t\t\tJLabel label = new JLabel(\"Hello from Swing\", SwingConstants.CENTER);\n\
    \t\t\tJButton button = new JButton(\"Click me\");\n\
    \t\t\tint[] clicks = {0};\n\
    \t\t\tbutton.addActionListener(e -> {\n\
    \t\t\t\tclicks[0]++;\n\
    \t\t\t\tlabel.setText(\"Clicked \" + clicks[0]);\n\
    \t\t\t\tSystem.out.println(\"java: clicked \" + clicks[0]);\n\
    \t\t\t});\n\
    \t\t\tframe.setLayout(new BorderLayout());\n\
    \t\t\tframe.add(label, BorderLayout.CENTER);\n\
    \t\t\tframe.add(button, BorderLayout.SOUTH);\n\
    \t\t\tframe.setSize(360, 200);\n\
    \t\t\tframe.setDefaultCloseOperation(JFrame.EXIT_ON_CLOSE);\n\
    \t\t\tframe.addWindowListener(new java.awt.event.WindowAdapter() {\n\
    \t\t\t\tpublic void windowClosing(java.awt.event.WindowEvent e) { System.out.println(\"java: window closing\"); }\n\
    \t\t\t});\n\
    \t\t\tframe.setVisible(true);\n\
    \t\t\tSystem.out.println(\"java: window shown\");\n\
    \t\t\tif (self) {\n\
    \t\t\t\tbutton.doClick();\n\
    \t\t\t\tSwingUtilities.invokeLater(() -> {\n\
    \t\t\t\t\tJDialog dialog = new JDialog(frame, \"Java dialog\", true);\n\
    \t\t\t\t\tdialog.add(new JLabel(\"A modal dialog\", SwingConstants.CENTER));\n\
    \t\t\t\t\tdialog.setSize(280, 150);\n\
    \t\t\t\t\tdialog.setDefaultCloseOperation(JDialog.DISPOSE_ON_CLOSE);\n\
    \t\t\t\t\tdialog.addWindowListener(new java.awt.event.WindowAdapter() {\n\
    \t\t\t\t\t\tpublic void windowOpened(java.awt.event.WindowEvent e) { System.out.println(\"java: dialog shown\"); }\n\
    \t\t\t\t\t\tpublic void windowClosed(java.awt.event.WindowEvent e) { System.out.println(\"java: dialog closed\"); }\n\
    \t\t\t\t\t});\n\
    \t\t\t\t\tdialog.setVisible(true);\n\
    \t\t\t\t});\n\
    \t\t\t\tnew javax.swing.Timer(180000, e -> System.exit(0)).start();\n\
    \t\t\t}\n\
    \t\t});\n\
    \t}\n\
    }\n";

/// Скачать (или взять из кеша) дистрибутив Rider и сверить хеш.
pub fn rider_archive() -> Result<PathBuf> {
    let cache = sdk::root().join("thirdparty/rider");
    fs::create_dir_all(&cache)?;
    let file = cache.join(RIDER_URL.rsplit('/').next().unwrap_or("rider.tar.gz"));
    if file.is_file() && file_digest(&file)? == RIDER_SHA256 {
        return Ok(file);
    }
    say!("> curl {RIDER_URL}");
    let status = std::process::Command::new("curl")
        .args(["--location", "--fail", "--silent", "--show-error", "--output"])
        .arg(&file)
        .arg(RIDER_URL)
        .status()
        .context("не удалось запустить curl")?;
    if !status.success() {
        bail!("не скачался {RIDER_URL} ({status})");
    }
    let got = file_digest(&file)?;
    if got != RIDER_SHA256 {
        bail!("хеш Rider не тот: приехало {got}, ожидалось {RIDER_SHA256}; файл оставлен: {}", file.display());
    }
    Ok(file)
}

/// SHA-256 файла — потоком: архив Rider в память целиком не кладём.
fn file_digest(path: &Path) -> Result<String> {
    let mut file = std::io::BufReader::new(fs::File::open(path)?);
    let mut hasher = fpk::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Запись архива, которую берёт [`stream_tar`].
pub enum Item {
    File { data: Vec<u8>, mode: u32 },
    Link(String),
}

/// Пройти `.tar.gz` потоком: `wanted(путь)` решает, читать ли запись; данные
/// остальных пропускаются, не попадая в память. Пути — как в архиве, без
/// ведущего `./`.
pub fn stream_tar(path: &Path, mut wanted: impl FnMut(&str) -> bool, mut take: impl FnMut(String, Item)) -> Result<()> {
    let file = std::io::BufReader::with_capacity(1 << 20, fs::File::open(path)?);
    let mut tar = flate2::read::GzDecoder::new(file);
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    loop {
        if tar.read_exact(&mut header).is_err() || header.iter().all(|byte| *byte == 0) {
            break;
        }
        let field = |from: usize, len: usize| {
            let raw = &header[from..from + len];
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(len);
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let octal = |from: usize, len: usize| -> Result<u64> {
            let text = field(from, len);
            let text = text.trim();
            if text.is_empty() {
                return Ok(0);
            }
            u64::from_str_radix(text, 8).with_context(|| format!("число в заголовке tar: {text:?}"))
        };
        let size = octal(124, 12)?;
        let mode = octal(100, 8)? as u32;
        let kind = header[156];
        let padded = size.next_multiple_of(512);
        let name = long_name.take().unwrap_or_else(|| {
            let prefix = field(345, 155);
            let name = field(0, 100);
            if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
        });
        let link = long_link.take().unwrap_or_else(|| field(157, 100));

        let meta = matches!(kind, b'x' | b'L' | b'K');
        let read = meta || (matches!(kind, b'0' | 0 | b'7') && wanted(&name));
        if !read {
            std::io::copy(&mut (&mut tar).take(padded), &mut std::io::sink())?;
            if kind == b'2' && wanted(&name) {
                take(name.trim_start_matches("./").to_string(), Item::Link(link));
            }
            continue;
        }
        let mut data = vec![0u8; size as usize];
        tar.read_exact(&mut data)?;
        std::io::copy(&mut (&mut tar).take(padded - size), &mut std::io::sink())?;
        match kind {
            b'x' => {
                let text = String::from_utf8_lossy(&data);
                for record in text.split('\n') {
                    let Some((_, pair)) = record.split_once(' ') else { continue };
                    if let Some(value) = pair.strip_prefix("path=") {
                        long_name = Some(value.to_string());
                    } else if let Some(value) = pair.strip_prefix("linkpath=") {
                        long_link = Some(value.to_string());
                    }
                }
            }
            b'L' => long_name = Some(String::from_utf8_lossy(&data).trim_end_matches('\0').to_string()),
            b'K' => long_link = Some(String::from_utf8_lossy(&data).trim_end_matches('\0').to_string()),
            _ => take(name.trim_start_matches("./").to_string(), Item::File { data, mode }),
        }
    }
    Ok(())
}

/// Ссылки внутри набора — копиями файлов, на которые ведут (в пакете ссылок
/// нет). Ссылка в никуда пропускается с именем в журнале.
pub fn resolve_links(files: &mut BTreeMap<String, (Vec<u8>, u32)>, links: &BTreeMap<String, String>) {
    for (path, target) in links {
        let mut at = path.clone();
        let mut target = target.clone();
        let mut found = None;
        for _ in 0..16 {
            let parent = at.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
            let next = if target.starts_with('/') { target.trim_start_matches('/').to_string() } else { normalize(&format!("{parent}/{target}")) };
            if let Some(file) = files.get(&next) {
                found = Some(file.clone());
                break;
            }
            match links.get(&next) {
                Some(further) => {
                    at = next;
                    target = further.clone();
                }
                None => break,
            }
        }
        match found {
            Some(file) => {
                files.insert(path.clone(), file);
            }
            None => say!("java: ссылка {path} никуда не ведёт — пропущена"),
        }
    }
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

/// Собрать `jbr-25.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Ok(());
    }
    let archive = rider_archive()?;

    // JBR из архива: `jbr/…` → `jbr/…` в пакете.
    let prefix = format!("{RIDER_TOP}jbr/");
    let mut jbr: BTreeMap<String, (Vec<u8>, u32)> = BTreeMap::new();
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    stream_tar(&archive, |path| path.starts_with(&prefix), |path, item| {
        let Some(rest) = path.strip_prefix(&prefix) else { return };
        let place = format!("jbr/{rest}");
        match item {
            Item::File { data, mode } => {
                jbr.insert(place, (data, mode));
            }
            Item::Link(target) => {
                links.insert(place, target);
            }
        }
    })?;
    resolve_links(&mut jbr, &links);
    if !jbr.contains_key("jbr/bin/java") {
        bail!("в архиве Rider нет {prefix}bin/java");
    }
    say!("java: из архива Rider — {} файлов JBR", jbr.len());

    // Библиотеки Debian по цепочке `DT_NEEDED`.
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut base: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &crate::debian::PACKAGES, &mut base)?;
    let mut extra: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut extra)?;

    let mut files: Vec<(String, Vec<u8>, u32)> = jbr.into_iter().map(|(path, (data, mode))| (path, data, mode)).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut todo: Vec<String> = ROOTS.iter().map(|name| String::from(*name)).collect();
    let mut from_base = 0;
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if LIB_DIRS.iter().any(|dir| follow(&base, &format!("{dir}/{name}"), 0).is_some()) {
            from_base += 1;
            continue;
        }
        let Some((data, mode)) = LIB_DIRS.iter().find_map(|dir| follow(&extra, &format!("{dir}/{name}"), 0)) else {
            bail!("библиотеки {name} нет ни в корне Debian, ни в пакетах для Java");
        };
        todo.extend(crate::chromium::needed(data).with_context(|| format!("{name}: не разобран ELF"))?);
        files.push((format!("lib/{name}"), data.clone(), mode));
    }
    say!("java: {} библиотек в пакет, {from_base} уже в корне Debian", seen.len() - from_base);

    // Настройка fontconfig из пакета (`conf.d`) не нужна: свой `fonts.conf`.
    for (path, node) in &extra {
        if let Some(name) = path.strip_prefix(FONT_DIR) {
            if matches!(node, Node::File { .. }) {
                if let Some((data, mode)) = follow(&extra, path, 0) {
                    files.push((format!("fonts/{name}"), data.clone(), mode));
                }
            }
        }
    }
    files.push((String::from("fonts/fonts.conf"), FONTS_CONF.as_bytes().to_vec(), 0o644));
    // Раскладки XKB — в `/opt/java/xkb` (`XKB_CONFIG_ROOT`); ссылки — копиями.
    let mut xkb = 0;
    for (path, node) in &extra {
        if let Some(name) = path.strip_prefix(XKB_DIR) {
            if matches!(node, Node::Dir) || name.is_empty() || !XKB_KEEP.iter().any(|keep| name == *keep || (keep.ends_with('/') && name.starts_with(keep))) {
                continue;
            }
            if let Some((data, mode)) = follow(&extra, path, 0) {
                files.push((format!("xkb/{name}"), data.clone(), mode));
                xkb += 1;
            }
        }
    }
    if xkb == 0 {
        bail!("в xkb-data нет {XKB_DIR}");
    }
    files.push((String::from("samples/Hello.java"), HELLO.as_bytes().to_vec(), 0o644));
    files.push((String::from("samples/Window.java"), WINDOW.as_bytes().to_vec(), 0o644));
    // Журнал AWT на Wayland — для разбора: `-Djava.util.logging.config.file=
    // /opt/java/samples/wl-log.properties`.
    files.push((String::from("samples/wl-log.properties"), WL_LOG.as_bytes().to_vec(), 0o644));

    // Запуск — сценарий bash: у `java` нет `RUNPATH` на наши библиотеки, а
    // окна — только через Wayland (`WLToolkit`): X11 в системе нет.
    let script = "#!/bin/bash\n\
        # Java (JetBrains Runtime 25): библиотеки и шрифты — рядом, окна через Wayland.\n\
        export LD_LIBRARY_PATH=/opt/java/lib\n\
        export FONTCONFIG_FILE=/opt/java/fonts/fonts.conf\n\
        export XKB_CONFIG_ROOT=/opt/java/xkb\n\
        export XDG_RUNTIME_DIR=/run/user/0\n\
        export WAYLAND_DISPLAY=wayland-0\n\
        export HOME=${HOME:-/root}\n\
        export TMPDIR=${TMPDIR:-$HOME}\n\
        exec /opt/java/jbr/bin/java -Dawt.toolkit.name=WLToolkit \"$@\"\n";
    files.push((String::from("bin/java"), script.as_bytes().to_vec(), 0o755));

    let mut builder = Builder::new(Kind::Package, "java", "25");
    builder.field("summary", "JetBrains Runtime 25 (Java) from the Rider distribution; AWT and Swing windows through the kernel Wayland server");
    builder.field("about", "Java 25 для программ Linux: консоль и окна Swing");
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
    say!("java: {} — {} файлов, {} МиБ", out.display(), files.len(), bytes / (1024 * 1024));
    Ok(())
}
