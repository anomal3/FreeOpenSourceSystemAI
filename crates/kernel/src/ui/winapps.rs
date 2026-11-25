//! Программы Windows, поставленные в префиксы Wine/Proton (21.11.2026).
//!
//! Роман поставил Notepad++ его установщиком и выбрал «ярлык на рабочем
//! столе» — ни на столе, ни в «Пуске» ничего не появилось. Журнал железа
//! (hw-log14) показал, что Wine своё сделал: установщик записал
//! `C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Notepad++.lnk` и
//! `C:\users\Public\Desktop\Notepad++.lnk`, а `winemenubuilder` по обоим
//! написал один и тот же `drive_c/proton_shortcuts/Notepad++.desktop` (второй
//! затёр первый: ярлык стола от пункта меню по нему не отличить).
//!
//! Поэтому опора — сами ярлыки Windows (`.lnk`), как у Проводника: они есть
//! всегда, что бы ни делал `winemenubuilder` (его выключают, он падает, его
//! `.desktop` пишет одно имя на оба ярлыка). Здесь — разбор `.lnk`
//! (MS-SHLLINK: путь цели из LinkInfo, из строк или из списка ID), перевод
//! пути Windows в путь префикса (без учёта регистра, как NTFS) и значок из
//! ресурсов цели. `.desktop` из `proton_shortcuts` — запасной путь для
//! ярлыков, которых в меню «Пуск» префикса нет.
//!
//! Где что лежит в префиксе:
//! - «Пуск» всех: `drive_c/ProgramData/Microsoft/Windows/Start Menu/Programs`;
//! - «Пуск» пользователя: `drive_c/users/<имя>/AppData/Roaming/Microsoft/Windows/Start Menu/Programs`;
//! - стол: `drive_c/users/Public/Desktop`, `drive_c/users/<имя>/Desktop`.
//!
//! Удалили программу — её ярлыки удаляет деинсталлятор; а если ярлык остался,
//! но цели на диске нет, пункт всё равно не показывается.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use mini_ui::image::Image;

use crate::vfs::NodeKind;

/// Программа Windows: ярлык и что из него известно.
#[derive(Clone, Debug)]
pub struct WinApp {
    /// Подпись — имя ярлыка без `.lnk` (так его показывает Windows).
    pub caption: String,
    /// Путь ярлыка: `.lnk` или `.desktop` `winemenubuilder`.
    pub file: String,
    /// Префикс Wine — каталог с `drive_c`.
    pub prefix: String,
    /// Путь цели в префиксе (пусто — не узнали: ярлык MSI и подобные).
    pub target: String,
    /// Значок: путь FIMG (настоящий или из [`remember_icon`]) или пусто.
    pub icon: String,
    /// Что отдать `fossai-proton --open=`: сама программа, если ярлык
    /// запускает `.exe` без ключей (тогда сценарий выбирает DXVK, TMNF, D2R
    /// так же, как для двойного щелчка по ней), иначе ярлык (`start /unix`).
    pub open: String,
    /// Ярлык удаления («Uninstall …»).
    pub uninstall: bool,
    /// Лежит на столе префикса (установщик попросил ярлык на рабочем столе).
    pub desktop: bool,
    /// Папка «Пуска» Windows, в которой лежит ярлык (`Notepad++`,
    /// `TmNationsForever`), пусто — корень «Программ».
    pub group: String,
}

impl WinApp {
    /// Аргументы `fossai-proton` для запуска: ярлык Windows — `--open=` (префикс
    /// сценарий берёт из пути), `.desktop` — `--shortcut=`.
    #[must_use]
    pub fn args(&self) -> String {
        if is_lnk(&self.file) {
            alloc::format!("--open={}", super::panel::pct(if self.open.is_empty() { &self.file } else { &self.open }))
        } else {
            alloc::format!("--shortcut={}", super::panel::pct(&self.file))
        }
    }

    /// Ключ строки «Пуска» (имя пакета-строки).
    #[must_use]
    pub fn key(&self) -> String {
        alloc::format!("windows:{}", self.file)
    }
}

fn is_lnk(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".lnk")
}

fn exists(path: &str) -> bool {
    matches!(crate::fs::read(path, 0), Some(Ok(_)))
}

fn list(path: &str) -> Vec<crate::vfs::DirEntry> {
    match crate::fs::list(path) {
        Some(Ok(entries)) => entries.into_iter().filter(|entry| entry.name != "." && entry.name != "..").collect(),
        _ => Vec::new(),
    }
}

/// Дома, в которых ищутся префиксы: того, кто вошёл, и `/root` (живая флешка
/// и `run` из оболочки ставят туда) — и их двойники в корне Debian
/// (`/opt/debian/root`, `/opt/debian/home/<имя>`). До 22.11 `mkdir -p` Wine
/// заводил такой двойник (`linux::fsops::place`), и программа Linux видела
/// свой дом там: префикс `~/.proton` лежал в `/opt/debian/root/.proton`
/// (hw-log15: Notepad++ не появлялся ни в «Пуске», ни на столе). На
/// установленной системе двойник с префиксом остаётся на диске — ищем и там.
pub(super) fn homes() -> Vec<String> {
    let mut homes = alloc::vec![super::context::home_dir()];
    if homes[0] != "/root" {
        homes.push(String::from("/root"));
    }
    for index in 0..homes.len() {
        let shadow = alloc::format!("{}{}", crate::linux_path::LINUX_ROOT, homes[index]);
        if matches!(crate::fs::list(&shadow), Some(Ok(_))) {
            homes.push(shadow);
        }
    }
    homes
}

/// Префиксы Wine: каталоги с точкой в домах, в которых есть `drive_c`.
#[must_use]
pub fn prefixes() -> Vec<String> {
    let mut out = Vec::new();
    for home in homes() {
        for entry in list(&home) {
            if entry.kind != NodeKind::Directory || !entry.name.starts_with('.') || entry.name.len() < 2 {
                continue;
            }
            let prefix = alloc::format!("{home}/{}", entry.name);
            if matches!(crate::fs::list(&alloc::format!("{prefix}/drive_c")), Some(Ok(_))) {
                out.push(prefix);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Запись `name` в каталоге `dir` без учёта регистра (NTFS так и ищет):
/// точное имя — сразу, иначе — по списку каталога.
fn child(dir: &str, name: &str) -> Option<String> {
    let exact = alloc::format!("{dir}/{name}");
    if exists(&exact) {
        return Some(exact);
    }
    let lower = name.to_lowercase();
    list(dir).into_iter().find(|entry| entry.name.to_lowercase() == lower).map(|entry| alloc::format!("{dir}/{}", entry.name))
}

/// Путь по частям от `base` без учёта регистра; `None` — чего-то нет.
fn walk(base: &str, parts: &[&str]) -> Option<String> {
    let mut at = String::from(base);
    for part in parts.iter().filter(|part| !part.is_empty() && **part != ".") {
        if *part == ".." {
            if let Some((parent, _)) = at.rsplit_once('/') {
                at = String::from(parent);
            }
            continue;
        }
        at = child(&at, part)?;
    }
    Some(at)
}

/// Пользователи префикса (`drive_c/users/*`, кроме `Public`).
fn users(prefix: &str) -> Vec<String> {
    list(&alloc::format!("{prefix}/drive_c/users"))
        .into_iter()
        .filter(|entry| entry.kind == NodeKind::Directory && !entry.name.eq_ignore_ascii_case("public"))
        .map(|entry| entry.name)
        .collect()
}

/// Раскрыть переменные окружения пути ярлыка (`%ProgramFiles%\…`).
fn expand(prefix: &str, path: &str) -> String {
    if !path.contains('%') {
        return String::from(path);
    }
    let user = users(prefix).into_iter().next().unwrap_or_else(|| String::from("steamuser"));
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = tail[..end].to_ascii_lowercase();
        let value = match name.as_str() {
            "programfiles" | "programw6432" => String::from("C:\\Program Files"),
            "programfiles(x86)" => String::from("C:\\Program Files (x86)"),
            "commonprogramfiles" => String::from("C:\\Program Files\\Common Files"),
            "systemroot" | "windir" => String::from("C:\\windows"),
            "systemdrive" => String::from("C:"),
            "programdata" | "allusersprofile" => String::from("C:\\ProgramData"),
            "public" => String::from("C:\\users\\Public"),
            "userprofile" => alloc::format!("C:\\users\\{user}"),
            "appdata" => alloc::format!("C:\\users\\{user}\\AppData\\Roaming"),
            "localappdata" => alloc::format!("C:\\users\\{user}\\AppData\\Local"),
            _ => alloc::format!("%{}%", &tail[..end]),
        };
        out.push_str(&value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Путь Windows (`C:\Program Files\…`) в путь префикса. Диск `C:` —
/// `drive_c`, прочие — `dosdevices/x:`. `None` — файла нет.
#[must_use]
pub fn to_unix(prefix: &str, path: &str) -> Option<String> {
    let path = expand(prefix, path.trim().trim_matches('"'));
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let letter = (bytes[0] as char).to_ascii_lowercase();
    let base = if letter == 'c' { alloc::format!("{prefix}/drive_c") } else { alloc::format!("{prefix}/dosdevices/{letter}:") };
    let parts: Vec<&str> = path[2..].split(['\\', '/']).collect();
    walk(&base, &parts)
}

// ── Разбор .lnk (MS-SHLLINK) ───────────────────────────────────────────────

/// Что достаётся из ярлыка.
#[derive(Default, Debug, Clone)]
pub struct Lnk {
    /// Путь цели (Windows).
    pub target: String,
    /// Путь относительно ярлыка (`.\…`), если цели в LinkInfo нет.
    pub relative: String,
    pub args: String,
    pub workdir: String,
    /// Файл значка (Windows) и номер в нём.
    pub icon: String,
    pub icon_index: i32,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

/// Строка ANSI до нуля (кодовая страница — как латиница; прочее — `?`).
fn ansi_z(b: &[u8], at: usize) -> String {
    b.get(at..).unwrap_or(&[]).iter().take_while(|byte| **byte != 0).map(|byte| if byte.is_ascii() { *byte as char } else { '?' }).collect()
}

/// Строка UTF-16 до нуля.
fn wide_z(b: &[u8], at: usize) -> String {
    let mut units = Vec::new();
    let mut i = at;
    while let Some(unit) = u16_at(b, i) {
        if unit == 0 {
            break;
        }
        units.push(unit);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

/// Разобрать `.lnk`. `None` — не ярлык.
#[must_use]
pub fn parse_lnk(b: &[u8]) -> Option<Lnk> {
    if u32_at(b, 0)? != 0x4C || b.get(4..8)? != [0x01, 0x14, 0x02, 0x00] {
        return None;
    }
    let flags = u32_at(b, 20)?;
    let mut lnk = Lnk { icon_index: u32_at(b, 56)? as i32, ..Lnk::default() };
    let unicode = flags & 0x80 != 0;
    let mut at = 76usize;
    let mut idlist_path = String::new();
    if flags & 0x01 != 0 {
        let size = usize::from(u16_at(b, at)?);
        idlist_path = idlist(b.get(at + 2..at + 2 + size)?);
        at += 2 + size;
    }
    if flags & 0x02 != 0 {
        let size = u32_at(b, at)? as usize;
        let info = b.get(at..at + size)?;
        let header = u32_at(info, 4)? as usize;
        let info_flags = u32_at(info, 8)?;
        if info_flags & 1 != 0 {
            let base = if header >= 0x24 && u32_at(info, 28)? != 0 { wide_z(info, u32_at(info, 28)? as usize) } else { ansi_z(info, u32_at(info, 16)? as usize) };
            let suffix = if header >= 0x24 && u32_at(info, 32)? != 0 { wide_z(info, u32_at(info, 32)? as usize) } else { ansi_z(info, u32_at(info, 24)? as usize) };
            lnk.target = base;
            if !suffix.is_empty() {
                if !lnk.target.ends_with('\\') {
                    lnk.target.push('\\');
                }
                lnk.target.push_str(&suffix);
            }
        }
        at += size;
    }
    // Строки: имя, относительный путь, рабочий каталог, аргументы, значок.
    let mut read_string = |present: bool| -> Option<String> {
        if !present {
            return Some(String::new());
        }
        let count = usize::from(u16_at(b, at)?);
        at += 2;
        let text = if unicode {
            let units: Vec<u16> = (0..count).filter_map(|i| u16_at(b, at + i * 2)).collect();
            at += count * 2;
            String::from_utf16_lossy(&units)
        } else {
            let text: String = b.get(at..at + count)?.iter().map(|byte| if byte.is_ascii() { *byte as char } else { '?' }).collect();
            at += count;
            text
        };
        Some(text)
    };
    let _name = read_string(flags & 0x04 != 0)?;
    lnk.relative = read_string(flags & 0x08 != 0)?;
    lnk.workdir = read_string(flags & 0x10 != 0)?;
    lnk.args = read_string(flags & 0x20 != 0)?;
    lnk.icon = read_string(flags & 0x40 != 0)?;
    // Блоки ExtraData: путь с переменными окружения (0xA0000001) и значок с
    // ними же (0xA0000007) — у ярлыков на `%ProgramFiles%`.
    while let (Some(size), Some(sig)) = (u32_at(b, at), u32_at(b, at + 4)) {
        if size < 8 {
            break;
        }
        let block = b.get(at..at + size as usize).unwrap_or(&[]);
        let pick = |block: &[u8]| -> String {
            let wide = wide_z(block, 268);
            if wide.is_empty() { ansi_z(block, 8) } else { wide }
        };
        match sig {
            0xA000_0001 if lnk.target.is_empty() => lnk.target = pick(block),
            0xA000_0007 if lnk.icon.is_empty() || lnk.icon.contains('%') => lnk.icon = pick(block),
            _ => {}
        }
        at += size as usize;
    }
    if lnk.target.is_empty() {
        lnk.target = idlist_path;
    }
    Some(lnk)
}

/// Путь из списка ID оболочки: элемент диска (`C:\`) и элементы файловой
/// системы (короткое имя ANSI с 14-го байта или длинное из блока `0xBEEF0004`).
fn idlist(b: &[u8]) -> String {
    let mut path = String::new();
    let mut at = 0usize;
    while let Some(size) = u16_at(b, at) {
        let size = usize::from(size);
        if size < 3 {
            break;
        }
        let item = b.get(at + 2..at + size).unwrap_or(&[]);
        let kind = item.first().copied().unwrap_or(0);
        if kind & 0x70 == 0x20 {
            // Диск: «C:\».
            let drive = ansi_z(item, 1);
            if drive.len() >= 2 {
                path = String::from(&drive[..2]);
            }
        } else if kind & 0x70 == 0x30 && !path.is_empty() {
            let short = ansi_z(item, 12);
            // Длинное имя — в блоке расширения в конце элемента.
            let long = item
                .windows(4)
                .position(|w| w == [0x04, 0x00, 0xEF, 0xBE])
                .and_then(|sig| {
                    let ext = sig.checked_sub(4)?;
                    let version = u16_at(item, ext + 2)?;
                    let offset = if version >= 9 { 46 } else if version >= 7 { 42 } else { 26 };
                    let name = wide_z(item, ext + offset);
                    (!name.is_empty()).then_some(name)
                })
                .unwrap_or(short);
            path.push('\\');
            path.push_str(&long);
        }
        at += size;
    }
    path
}

// ── Значки ─────────────────────────────────────────────────────────────────

/// Найденные значки: ключ «файл|номер» и путь картинки (`None` — значка нет,
/// второй раз не искать). Картинка лежит в кэше значков стола
/// ([`super::appicon::remember`]) под путём `/winicon/…`: на диск ничего не
/// пишется (живая флешка, чужой префикс).
static ICONS: crate::sync::SpinLock<Vec<(String, Option<(String, Arc<Image>)>)>> = crate::sync::SpinLock::new(Vec::new());

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3))
}

/// Значок из файла `path` (`.exe`, `.dll`, `.ico`) номер `index`: путь для
/// `appicon` или пусто.
#[must_use]
pub fn remember_icon(path: &str, index: i32) -> String {
    let key = alloc::format!("{path}|{index}");
    let known = ICONS.lock().iter().find(|(known, _)| *known == key).map(|(_, found)| found.clone());
    let found = match known {
        Some(found) => found,
        None => {
            // Читается без замка: файл может ждать диска.
            let image = super::launch::icon_image(path, index).map(Arc::new);
            let found = image.map(|image| (alloc::format!("/winicon/{:016x}.fimg", fnv(&key)), image));
            ICONS.lock().push((key, found.clone()));
            found
        }
    };
    match found {
        Some((name, image)) => {
            // Кэш значков стола забывают при смене пакетов — положить снова.
            super::appicon::remember(&name, image);
            name
        }
        None => String::new(),
    }
}

/// Картинка значка по его условному пути `/winicon/…` — из своего списка
/// (кэш стола забывают при смене пакетов; «Открыть с помощью», 22.11).
#[must_use]
pub fn icon_by_name(name: &str) -> Option<Arc<Image>> {
    ICONS.lock().iter().find_map(|(_, found)| found.as_ref().filter(|(known, _)| known == name).map(|(_, image)| image.clone()))
}

/// Значок ярлыка: поле значка, иначе цель.
fn lnk_icon(prefix: &str, lnk: &Lnk, target: &str) -> String {
    if !lnk.icon.is_empty() {
        if let Some(file) = to_unix(prefix, &lnk.icon) {
            let icon = remember_icon(&file, lnk.icon_index);
            if !icon.is_empty() {
                return icon;
            }
        }
    }
    let lower = target.to_ascii_lowercase();
    if [".exe", ".dll", ".ico"].iter().any(|ext| lower.ends_with(ext)) {
        return remember_icon(target, if lnk.icon.is_empty() { lnk.icon_index } else { 0 });
    }
    String::new()
}

/// Значок `.lnk` по пути (заставка запуска, ярлык на столе): префикс — из
/// пути ярлыка, иначе первый, где нашлась цель.
#[must_use]
pub fn icon_of_lnk(path: &str) -> String {
    let Some((lnk, prefix, target)) = read_lnk(path) else {
        return String::new();
    };
    lnk_icon(&prefix, &lnk, &target)
}

/// Префикс, в котором лежит путь (`…/.proton/drive_c/…`).
fn prefix_of(path: &str) -> Option<String> {
    let at = path.find("/drive_c/")?;
    Some(String::from(&path[..at]))
}

/// Прочитать ярлык и найти цель: (ярлык, префикс, путь цели или пусто).
fn read_lnk(path: &str) -> Option<(Lnk, String, String)> {
    let Some(Ok((bytes, _))) = crate::fs::read(path, 256 * 1024) else {
        return None;
    };
    let lnk = parse_lnk(&bytes)?;
    let mut candidates = Vec::new();
    if let Some(prefix) = prefix_of(path) {
        candidates.push(prefix);
    }
    for prefix in prefixes() {
        if !candidates.contains(&prefix) {
            candidates.push(prefix);
        }
    }
    for prefix in &candidates {
        if let Some(target) = target_of(prefix, path, &lnk) {
            return Some((lnk, prefix.clone(), target));
        }
    }
    let prefix = candidates.into_iter().next().unwrap_or_default();
    Some((lnk, prefix, String::new()))
}

/// Цель ярлыка в префиксе.
///
/// Цель без расширения (`{app}\Updater` у ярлыка стола установщика Rupor,
/// 24.11) Windows запускает как `Updater.exe` — `CreateProcess` и
/// `ShellExecute` дописывают `.exe` сами. Без этого ярлык считался ярлыком
/// удалённой программы, и значка на столе не было.
fn target_of(prefix: &str, lnk_path: &str, lnk: &Lnk) -> Option<String> {
    if !lnk.target.is_empty() {
        if let Some(found) = to_unix(prefix, &lnk.target) {
            // Каталог с тем же именем, что программа без расширения, — не цель.
            if !(no_extension(&lnk.target) && matches!(crate::fs::list(&found), Some(Ok(_)))) {
                return Some(found);
            }
        }
        if no_extension(&lnk.target) {
            for ext in [".exe", ".com", ".bat", ".cmd"] {
                if let Some(found) = to_unix(prefix, &alloc::format!("{}{ext}", lnk.target.trim().trim_matches('"'))) {
                    return Some(found);
                }
            }
        }
    }
    if !lnk.relative.is_empty() && lnk_path.starts_with(prefix) {
        let dir = lnk_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let parts: Vec<&str> = lnk.relative.split(['\\', '/']).collect();
        return walk(dir, &parts);
    }
    None
}

/// Последняя часть пути Windows без точки (`…\Updater`).
fn no_extension(path: &str) -> bool {
    let name = path.trim().trim_matches('"').rsplit(['\\', '/']).next().unwrap_or("");
    !name.is_empty() && !name.contains('.')
}

/// Строка запуска ярлыка `.lnk`, лежащего где угодно (на столе человека):
/// префикс — где нашлась его цель. `None` — нет пакета `proton`.
#[must_use]
pub fn open_line(path: &str) -> Option<String> {
    if !proton_present() {
        return None;
    }
    let open = alloc::format!("--open={}", super::panel::pct(path));
    if is_lnk(path) && prefix_of(path).is_none() {
        if let Some((_, prefix, target)) = read_lnk(path) {
            if !target.is_empty() && !prefix.contains(' ') {
                return Some(super::panel::proton_line(&alloc::format!("--env=WINEPREFIX={prefix} {open}")));
            }
        }
    }
    Some(super::panel::proton_line(&open))
}

use super::panel::PROTON_SCRIPT;

/// Считать пакет `proton` стоящим (`winapps assume-proton` оболочки): стенд
/// проверяет «Программы Windows» на живой системе стенда, где `/opt` только
/// для чтения и пакета нет.
static ASSUME_PROTON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn assume_proton() {
    ASSUME_PROTON.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Стоит ли пакет `proton`.
#[must_use]
pub fn proton_present() -> bool {
    ASSUME_PROTON.load(core::sync::atomic::Ordering::Relaxed) || exists(PROTON_SCRIPT)
}

/// Файл, который открывается через Proton: ярлык, программа, установщик.
#[must_use]
pub fn is_windows_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".lnk", ".exe", ".msi", ".bat", ".cmd"].iter().any(|ext| lower.ends_with(ext))
}

// ── Обход префиксов ────────────────────────────────────────────────────────

/// Ярлык удаления по имени или цели.
fn is_uninstall(caption: &str, target: &str) -> bool {
    let lower = caption.to_lowercase();
    let exe = target.rsplit('/').next().unwrap_or("").to_lowercase();
    lower.starts_with("uninstall") || lower.contains(" uninstall") || lower.starts_with("удалить") || lower.starts_with("удаление") || exe.starts_with("uninst") || exe.starts_with("unins0")
}

/// Ярлыки `.lnk` каталога `dir` и его подкаталогов (до глубины 3): путь и
/// подпапка «Пуска» (первый уровень).
fn lnk_files(dir: &str, group: &str, depth: u32, out: &mut Vec<(String, String)>) {
    for entry in list(dir) {
        let path = alloc::format!("{dir}/{}", entry.name);
        match entry.kind {
            NodeKind::Directory if depth < 3 => {
                let group = if group.is_empty() { entry.name.as_str() } else { group };
                lnk_files(&path, group, depth + 1, out);
            }
            NodeKind::File if is_lnk(&entry.name) =>out.push((path, String::from(group))),
            _ => {}
        }
    }
}

/// Каталоги «Пуска» и стола префикса: (каталог, это стол).
fn roots(prefix: &str) -> Vec<(String, bool)> {
    let c = alloc::format!("{prefix}/drive_c");
    let mut out = Vec::new();
    let mut push = |parts: &[&str], desktop: bool| {
        if let Some(dir) = walk(&c, parts) {
            out.push((dir, desktop));
        }
    };
    push(&["ProgramData", "Microsoft", "Windows", "Start Menu", "Programs"], false);
    push(&["users", "Public", "Desktop"], true);
    for user in users(prefix) {
        push(&["users", &user, "AppData", "Roaming", "Microsoft", "Windows", "Start Menu", "Programs"], false);
        // Старые префиксы (Wine до 5.x).
        push(&["users", &user, "Start Menu", "Programs"], false);
        push(&["users", &user, "Desktop"], true);
    }
    out
}

/// Все программы Windows всех префиксов: пункты «Пуска» и ярлыки столов
/// префиксов (`desktop`), ярлыки удаления (`uninstall`). Без пакета `proton`
/// — пусто: запускать нечем.
#[must_use]
pub fn scan() -> Vec<WinApp> {
    let mut out: Vec<WinApp> = Vec::new();
    if !proton_present() {
        return out;
    }
    // Стол человека: ярлык с тем же именем там уже лежит (Wine мог связать
    // стол префикса со столом дома) — второй раз не показывать.
    let home_desk = super::context::desktop_dir();
    let home_names: Vec<String> = list(&home_desk).into_iter().map(|entry| entry.name.to_lowercase()).collect();
    for prefix in prefixes() {
        let mut seen_files: Vec<String> = Vec::new();
        for (root, desktop) in roots(&prefix) {
            let mut files = Vec::new();
            lnk_files(&root, "", 0, &mut files);
            for (file, group) in files {
                if seen_files.contains(&file) {
                    continue;
                }
                seen_files.push(file.clone());
                let name = file.rsplit('/').next().unwrap_or(&file);
                if desktop && home_names.iter().any(|known| *known == name.to_lowercase()) {
                    continue;
                }
                let Some(Ok((bytes, _))) = crate::fs::read(&file, 256 * 1024) else { continue };
                let Some(lnk) = parse_lnk(&bytes) else { continue };
                let target = target_of(&prefix, &file, &lnk);
                // Цель названа, но её нет — программу удалили, ярлык остался.
                if target.is_none() && (!lnk.target.is_empty() || !lnk.relative.is_empty()) && !lnk.target.contains('%') {
                    continue;
                }
                let target = target.unwrap_or_default();
                let caption = String::from(name[..name.len() - 4].trim());
                let icon = lnk_icon(&prefix, &lnk, &target);
                let direct = lnk.args.trim().is_empty() && target.to_ascii_lowercase().ends_with(".exe");
                out.push(WinApp {
                    open: if direct { target.clone() } else { file.clone() },
                    uninstall: is_uninstall(&caption, &target),
                    caption,
                    file,
                    prefix: prefix.clone(),
                    target,
                    icon,
                    desktop,
                    group: if desktop { String::new() } else { group },
                });
            }
        }
        // Ярлыки `winemenubuilder` (P12) — только те, чьих `.lnk` нет.
        let dir = alloc::format!("{prefix}/drive_c/proton_shortcuts");
        for entry in list(&dir) {
            if entry.kind != NodeKind::File || !entry.name.ends_with(".desktop") {
                continue;
            }
            let path = alloc::format!("{dir}/{}", entry.name);
            let Some(Ok((bytes, _))) = crate::fs::read(&path, 16 * 1024) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            let field = |key: &str| text.lines().find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('='))).map(str::trim);
            if field("NoDisplay") == Some("true") {
                continue;
            }
            let caption = String::from(field("Name").unwrap_or_else(|| entry.name.trim_end_matches(".desktop")));
            // И ярлык стола тоже (24.11, Rupor: `Rupor.exe.desktop` от ярлыка
            // стола был лишней строкой «Пуска» рядом со значком на столе).
            if out.iter().any(|app| app.prefix == prefix && app.caption.eq_ignore_ascii_case(&caption)) {
                continue;
            }
            let icon = super::panel::shortcut_icon(&dir, field("Icon").unwrap_or(""));
            let lower = caption.to_lowercase();
            out.push(WinApp {
                open: String::new(),
                uninstall: lower.starts_with("uninstall") || lower.starts_with("удалить") || entry.name.to_lowercase().starts_with("uninstall"),
                caption,
                file: path,
                prefix: prefix.clone(),
                target: String::new(),
                icon,
                desktop: false,
                group: String::new(),
            });
        }
    }
    out.sort_by(|a, b| a.caption.to_lowercase().cmp(&b.caption.to_lowercase()));
    // Итог — в журнал, когда он меняется (hw-log15: обход молчал, и по
    // журналу нельзя было понять, нашёл ли он префикс).
    let found = prefixes();
    let summary = alloc::format!(
        "{} prefix(es) [{}], {} app(s): {} in Start, {} on the desktop, {} uninstall",
        found.len(),
        found.join(", "),
        out.len(),
        out.iter().filter(|app| !app.desktop && !app.uninstall).count(),
        out.iter().filter(|app| app.desktop).count(),
        out.iter().filter(|app| app.uninstall).count()
    );
    let mut last = LAST_SCAN.lock();
    if *last != summary {
        crate::kprintln!("  winapps     : {summary}");
        *last = summary;
    }
    drop(last);
    out
}

/// Итог последнего обхода (для журнала — только перемены).
static LAST_SCAN: crate::sync::SpinLock<String> = crate::sync::SpinLock::new(String::new());

/// Подпись «что поставлено» для стола: имена и размеры записей каталогов
/// «Пуска» и столов префиксов (верхний уровень). Сменилась — значки стола и
/// «Пуск» перечитываются (`Icons::packages_changed`).
#[must_use]
pub fn signature() -> String {
    let mut out = String::new();
    if !proton_present() {
        return out;
    }
    for prefix in prefixes() {
        for (root, _) in roots(&prefix) {
            out.push_str(&root);
            for entry in list(&root) {
                out.push_str(&alloc::format!(";{}:{}", entry.name, entry.size));
            }
            out.push('|');
        }
        for entry in list(&alloc::format!("{prefix}/drive_c/proton_shortcuts")) {
            out.push_str(&alloc::format!(";{}:{}", entry.name, entry.size));
        }
    }
    out
}

/// Положить копию ярлыка программы на стол человека (`~/Desktop/<имя>.lnk`):
/// «Пуск» → «Создать ярлык на рабочем столе» и перенос строки на стол.
/// `.desktop` `winemenubuilder` копируется как есть. Возвращает путь копии.
pub fn copy_to_desktop(file: &str, caption: &str) -> Result<String, String> {
    let Some(Ok((bytes, _))) = crate::fs::read(file, 256 * 1024) else {
        return Err(alloc::format!("{file}: не прочитать"));
    };
    let dir = super::context::desktop_dir();
    super::context::ensure_dir(&dir)?;
    let ext = if is_lnk(file) { "lnk" } else { "desktop" };
    let clean: String = caption.chars().map(|ch| if ch == '/' { '-' } else { ch }).collect();
    let mut path = alloc::format!("{dir}/{clean}.{ext}");
    let mut n = 2;
    while exists(&path) {
        path = alloc::format!("{dir}/{clean} ({n}).{ext}");
        n += 1;
    }
    let node = match crate::fs::create_as(crate::user::session::credentials(), &path, 0o644) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return Err(alloc::format!("{path}: {err}")),
        None => return Err(String::from("no filesystem is mounted")),
    };
    node.write_at(0, &bytes).map_err(|err| alloc::format!("{path}: {err}"))?;
    Ok(path)
}

/// Ярлык Windows на программу `target` (путь Windows) — так, как его пишет
/// Wine (`IShellLinkW::Save`): LinkInfo с путём ANSI, рабочий каталог и
/// значок строками Unicode. Для команды оболочки `mklnk` (сценарий стенда
/// `winapps` строит из неё префикс без установки Proton).
#[must_use]
pub fn build_lnk(target: &str) -> Vec<u8> {
    let mut b = alloc::vec![0u8; 76];
    b[0] = 0x4C;
    b[4..20].copy_from_slice(&[0x01, 0x14, 0x02, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
    let flags: u32 = 0x02 | 0x10 | 0x40 | 0x80;
    b[20..24].copy_from_slice(&flags.to_le_bytes());
    b[60..64].copy_from_slice(&1u32.to_le_bytes());
    let base: Vec<u8> = target.chars().map(|ch| if ch.is_ascii() { ch as u8 } else { b'?' }).collect();
    let mut info = alloc::vec![0u8; 28];
    let size = 28 + base.len() + 1 + 1;
    info[0..4].copy_from_slice(&(size as u32).to_le_bytes());
    info[4..8].copy_from_slice(&28u32.to_le_bytes());
    info[8..12].copy_from_slice(&1u32.to_le_bytes());
    info[16..20].copy_from_slice(&28u32.to_le_bytes());
    info[24..28].copy_from_slice(&((28 + base.len() + 1) as u32).to_le_bytes());
    info.extend_from_slice(&base);
    info.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&info);
    let dir = target.rsplit_once('\\').map_or("", |(dir, _)| dir);
    for text in [dir, target] {
        let units: Vec<u16> = text.encode_utf16().collect();
        b.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for unit in units {
            b.extend_from_slice(&unit.to_le_bytes());
        }
    }
    b.extend_from_slice(&0u32.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ярлык, который пишет Wine (`IShellLinkW::Save`): LinkInfo с путём,
    /// строки Unicode.
    #[test]
    fn parses_wine_lnk() {
        let mut b = alloc::vec![0u8; 76];
        b[0] = 0x4C;
        b[4..8].copy_from_slice(&[0x01, 0x14, 0x02, 0x00]);
        let flags: u32 = 0x02 | 0x10 | 0x40 | 0x80;
        b[20..24].copy_from_slice(&flags.to_le_bytes());
        let base = b"C:\\Program Files\\Notepad++\\notepad++.exe\0";
        let info_len = 28 + base.len() + 1;
        let mut info = alloc::vec![0u8; 28];
        info[0..4].copy_from_slice(&(info_len as u32).to_le_bytes());
        info[4..8].copy_from_slice(&28u32.to_le_bytes());
        info[8..12].copy_from_slice(&1u32.to_le_bytes());
        info[16..20].copy_from_slice(&28u32.to_le_bytes());
        info[24..28].copy_from_slice(&((28 + base.len()) as u32).to_le_bytes());
        info.extend_from_slice(base);
        info.push(0);
        b.extend_from_slice(&info);
        for text in ["C:\\Program Files\\Notepad++", "C:\\Program Files\\Notepad++\\notepad++.exe"] {
            let units: Vec<u16> = text.encode_utf16().collect();
            b.extend_from_slice(&(units.len() as u16).to_le_bytes());
            for unit in units {
                b.extend_from_slice(&unit.to_le_bytes());
            }
        }
        b.extend_from_slice(&0u32.to_le_bytes());
        let lnk = parse_lnk(&b).expect("lnk");
        assert_eq!(lnk.target, "C:\\Program Files\\Notepad++\\notepad++.exe");
        assert_eq!(lnk.workdir, "C:\\Program Files\\Notepad++");
        assert_eq!(lnk.icon, "C:\\Program Files\\Notepad++\\notepad++.exe");
    }

    #[test]
    fn extensionless_target() {
        assert!(no_extension("C:\\users\\steamuser\\AppData\\Local\\Rupor\\Updater"));
        assert!(no_extension("\"C:\\Rupor\\Updater\" "));
        assert!(!no_extension("C:\\Rupor\\Rupor.exe"));
        assert!(!no_extension("C:\\Rupor\\"));
    }
}
