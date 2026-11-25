//! «Редактор конфигурации» (фаза П3, 20.11): вместо реестра Windows.
//!
//! # Зачем
//!
//! Реестра у нас нет и не будет: настройки системы — текстовые файлы в `/etc`
//! (`services`, `desktop.cfg`, `httpd.cfg`, `update.cfg`, `passwd`…), а их
//! эталоны приезжают с образом в `/usr/share/defaults/etc`. Человеку, который
//! в Windows открыл бы regedit, здесь нужен один инструмент: увидеть все
//! такие файлы, открыть, поправить и **не сломать** — поэтому у известных
//! файлов есть проверка строк и описание ключа под кареткой.
//!
//! # Как устроено
//!
//! Слева — файлы `/etc` (и один уровень подкаталогов) и эталоны, которых в
//! `/etc` нет (пометка «эталон»: сохранение создаёт `/etc/<имя>`). Справа —
//! простой текстовый редактор, снизу — найденные проблемы и описание ключа.
//! Сохранение кладёт рядом прежнюю версию (`<файл>.bak`); файл с ошибками
//! сохраняется только по второму Ctrl+S. «Вернуть эталон» удаляет правку в
//! `/etc`, и система снова читает эталон.
//!
//! # Чего нет
//!
//! Выделения, буфера обмена и отмены по шагам: Ctrl+Z возвращает текст к
//! сохранённому. Для длинной правки есть «Блокнот».

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_abi::WIN_MOD_CTRL;
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, Dirent, KIND_DIRECTORY, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT,
    WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, open, open_write,
    println, read, readdir, remove, stat, write,
};

const SPEC: Spec = Spec { name: "cfgedit", title: "Configuration Editor", period_ms: 5_000 };

const ETC: &str = "/etc";
const DEFAULTS: &str = "/usr/share/defaults/etc";

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const TAB: u32 = '\t' as u32;
const DELETE_ASCII: u32 = 0x7F;
const CTRL_S: u32 = 0x13;
const CTRL_Z: u32 = 0x1A;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;
const F6: u32 = user_abi::WIN_KEY_F1 + 5;

/// Сколько байт файла читать. Настройки — килобайты; больше — не настройка.
const READ_LIMIT: usize = 256 * 1024;
/// Сколько читать при обходе списка (подсчёт проблем).
const SCAN_LIMIT: usize = 64 * 1024;
/// Сколько служб ведёт `/bin/init` (`MAX_SERVICES` там).
const INIT_MAX: usize = 8;

const LIST_ROW_H: u32 = 48;
const LIST_W: u32 = 290;
const INFO_H: u32 = 160;

// ---------------------------------------------------------------------------
// Известные файлы и их проверка
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Services,
    Autostart,
    Desktop,
    Httpd,
    Update,
    Passwd,
    Group,
    Hostname,
    Plain,
}

impl Kind {
    fn of(name: &str) -> Self {
        match name {
            "services" => Self::Services,
            "autostart" => Self::Autostart,
            "desktop.cfg" => Self::Desktop,
            "httpd.cfg" => Self::Httpd,
            "update.cfg" => Self::Update,
            "passwd" => Self::Passwd,
            "group" => Self::Group,
            "hostname" => Self::Hostname,
            _ => Self::Plain,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Services => "службы init",
            Self::Autostart => "автозагрузка",
            Self::Desktop => "настройки стола",
            Self::Httpd => "веб-сервер",
            Self::Update => "источники обновлений",
            Self::Passwd => "учётные записи",
            Self::Group => "группы",
            Self::Hostname => "имя компьютера",
            Self::Plain => "без проверки",
        }
    }
}

/// Ключи `desktop.cfg` (их пишет и читает стол: `ui/prefs.rs`, `deskcfg.rs`,
/// `look.rs`, `sysconf`) и что они значат.
const DESKTOP_KEYS: &[(&str, &str)] = &[
    ("theme", "тема: light или dark"),
    ("accent", "цвет акцента окон и кнопок"),
    ("wallpaper", "обои рабочего стола"),
    ("screen_off", "через сколько гасить экран: секунды, 10m или never"),
    ("titlebar", "высота заголовка окна: 36, 44 или 52"),
    ("scale", "масштаб интерфейса: auto, 100, 125, 150, 175, 200"),
    ("icon_places", "места значков на рабочем столе"),
    ("hidden_shortcuts", "ярлыки, убранные со стола"),
    ("open_with", "какой программой открывать типы файлов"),
    ("outputs", "раскладка мониторов"),
    ("volume", "громкость: 0..100"),
    ("panel_uptime", "время работы на панели: on или off"),
    ("panel_date", "дата у часов: on или off"),
    ("panel_labels", "подписи кнопок панели: on или off"),
    ("animations", "анимации окон: on или off (строка look= главнее)"),
    ("look", "оформление: набор и эффекты окон"),
    ("depth", "глубина цвета: 32, 16, 8 или 4"),
    ("taskbar", "панель задач: вид, край, автоскрытие, значки"),
    ("start", "«Пуск»: ширина и разделы"),
    ("start_pins", "закреплённое в «Пуске»"),
    ("taskbar_pins", "закреплённое на панели задач"),
    ("keyboard", "раскладки клавиатуры"),
    ("timezone", "часовой пояс: UTC+03:00"),
    ("language", "язык интерфейса"),
];

const HTTPD_KEYS: &[(&str, &str)] = &[
    ("port", "порт веб-сервера: 1..65535 (по умолчанию 8080)"),
    ("root", "каталог сайта"),
    ("mount", "подключить каталог: mount=/префикс /каталог (можно повторять)"),
    ("proxy", "передать префикс другому серверу: proxy=/префикс адрес порт"),
];

const UPDATE_KEYS: &[(&str, &str)] = &[
    ("server", "репозиторий: адрес или полный http(s)://…/ (можно повторять)"),
    ("port", "порт предыдущего server= (простая форма)"),
    ("path", "путь на предыдущем server= (простая форма)"),
];

#[derive(Clone)]
struct Problem {
    /// Строка, с единицы; ноль — файл целиком.
    line: usize,
    text: String,
    bad: bool,
}

fn bad(line: usize, text: impl Into<String>) -> Problem {
    Problem { line, text: text.into(), bad: true }
}

fn warn(line: usize, text: impl Into<String>) -> Problem {
    Problem { line, text: text.into(), bad: false }
}

fn validate(kind: Kind, lines: &[String]) -> Vec<Problem> {
    match kind {
        Kind::Services => validate_services(lines),
        Kind::Autostart => validate_autostart(lines),
        Kind::Desktop => validate_keys(lines, DESKTOP_KEYS, true),
        Kind::Httpd => validate_keys(lines, HTTPD_KEYS, false),
        Kind::Update => validate_keys(lines, UPDATE_KEYS, false),
        Kind::Passwd => validate_passwd(lines),
        Kind::Group => validate_group(lines),
        Kind::Hostname => validate_hostname(lines),
        Kind::Plain => Vec::new(),
    }
}

fn exists(path: &str) -> bool {
    let mut info = user_abi::Stat::default();
    stat(path, &mut info) >= 0 && info.kind != KIND_DIRECTORY
}

fn validate_services(lines: &[String]) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut active = 0usize;
    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        let (text, disabled) = match line.strip_prefix("#off ") {
            Some(rest) => (rest.trim(), true),
            None if line.is_empty() || line.starts_with('#') => continue,
            None => (line, false),
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() < 2 {
            out.push(bad(n, "нужно: имя путь [uid] [gid]"));
            continue;
        }
        if fields.len() > 4 {
            out.push(bad(n, "лишние поля: init читает имя, путь, uid и gid"));
        }
        let (name, exec) = (fields[0], fields[1]);
        if name.len() > 24 {
            out.push(bad(n, "имя службы длиннее 24 знаков — init её пропустит"));
        }
        if !exec.starts_with('/') {
            out.push(bad(n, "путь к программе должен быть абсолютным (/bin/…)"));
        } else if exec.len() > 96 {
            out.push(bad(n, "путь длиннее 96 знаков — init его пропустит"));
        } else if !exists(exec) {
            out.push(bad(n, format!("нет программы «{exec}»")));
        }
        for (field, what) in fields.iter().skip(2).zip(["uid", "gid"]) {
            if field.parse::<u32>().is_err() {
                out.push(bad(n, format!("{what} «{field}» — не число")));
            }
        }
        if fields.len() == 3 {
            out.push(warn(n, "gid не указан — будет равен uid"));
        }
        if names.iter().any(|known| known == name) {
            out.push(bad(n, format!("служба «{name}» уже описана выше")));
        } else {
            names.push(name.to_string());
        }
        if !disabled {
            active += 1;
            if active == INIT_MAX + 1 {
                out.push(bad(n, format!("init ведёт не больше {INIT_MAX} служб: эта и дальше не запустятся")));
            }
        }
    }
    out
}

fn validate_autostart(lines: &[String]) -> Vec<Problem> {
    let mut out = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, command) = line.split_once(char::is_whitespace).map_or((line, ""), |(w, c)| (w, c.trim()));
        if word != "on" && word != "off" {
            out.push(bad(n, "строка начинается с on или off"));
            continue;
        }
        let Some(program) = command.split_whitespace().next() else {
            out.push(bad(n, "после on/off нужна команда"));
            continue;
        };
        let path = if program.starts_with('/') { program.to_string() } else { format!("/bin/{program}") };
        if !exists(&path) {
            out.push(warn(n, format!("нет программы «{path}»")));
        }
    }
    out
}

fn validate_keys(lines: &[String], known: &[(&str, &str)], unique: bool) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            out.push(bad(n, "нужно ключ=значение"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() {
            out.push(bad(n, "пустой ключ перед «=»"));
            continue;
        }
        if !known.iter().any(|(name, _)| *name == key) {
            out.push(warn(n, format!("неизвестный ключ «{key}» — его никто не читает")));
            continue;
        }
        if unique {
            if seen.iter().any(|done| done == key) {
                out.push(warn(n, format!("ключ «{key}» уже был выше: действует первый")));
            } else {
                seen.push(key.to_string());
            }
        }
        if let Some(problem) = check_value(n, key, value) {
            out.push(problem);
        }
    }
    out
}

fn check_value(n: usize, key: &str, value: &str) -> Option<Problem> {
    let one_of = |allowed: &[&str]| -> Option<Problem> {
        (!allowed.contains(&value)).then(|| bad(n, format!("«{key}» принимает: {}", allowed.join(", "))))
    };
    match key {
        "theme" => one_of(&["light", "dark"]),
        "titlebar" => one_of(&["36", "44", "52"]),
        "scale" => one_of(&["auto", "100", "125", "150", "175", "200"]),
        "depth" => one_of(&["32", "16", "8", "4"]),
        "panel_uptime" | "panel_date" | "panel_labels" | "animations" => one_of(&["on", "off"]),
        "volume" => match value.parse::<u32>() {
            Ok(volume) if volume <= 100 => None,
            _ => Some(bad(n, "громкость — число 0..100")),
        },
        "screen_off" => {
            let ok = value.eq_ignore_ascii_case("never")
                || value.eq_ignore_ascii_case("off")
                || value.strip_suffix(['m', 'M']).unwrap_or(value).trim().parse::<u32>().is_ok();
            (!ok).then(|| bad(n, "секунды, 10m или never"))
        }
        "port" => match value.parse::<u32>() {
            Ok(port) if (1..=65535).contains(&port) => None,
            _ => Some(bad(n, "порт — число 1..65535")),
        },
        "root" if !value.starts_with('/') => Some(bad(n, "каталог должен быть абсолютным путём")),
        "mount" => (value.split_whitespace().count() != 2 || !value.starts_with('/'))
            .then(|| bad(n, "нужно: mount=/префикс /каталог")),
        "proxy" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            (parts.len() != 3 || !parts[0].starts_with('/') || parts[2].parse::<u16>().is_err())
                .then(|| bad(n, "нужно: proxy=/префикс адрес порт"))
        }
        "server" if value.contains("://") && !value.ends_with('/') => {
            Some(warn(n, "полный адрес должен кончаться «/»"))
        }
        _ => None,
    }
}

fn validate_passwd(lines: &[String]) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        // Своя строка — `имя:uid:gid:права:дом:алгоритм:соль:отпечаток`
        // (установщик), строка Linux — семь полей.
        if fields.len() != 8 && fields.len() != 7 {
            out.push(bad(n, format!("полей {} — нужно 8 (имя:uid:gid:права:дом:алгоритм:соль:отпечаток) или 7 (как в Linux)", fields.len())));
            continue;
        }
        let name = fields[0];
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_graphic()) {
            out.push(bad(n, "имя пустое или с непечатными знаками"));
        }
        let (uid_at, gid_at) = if fields.len() == 7 { (2, 3) } else { (1, 2) };
        if fields[uid_at].parse::<u32>().is_err() {
            out.push(bad(n, format!("uid «{}» — не число", fields[uid_at])));
        }
        if fields[gid_at].parse::<u32>().is_err() {
            out.push(bad(n, format!("gid «{}» — не число", fields[gid_at])));
        }
        if fields.len() == 8 && fields[uid_at] == "0" {
            out.push(warn(n, "uid 0 (root) ядро как учётную запись сеанса не принимает"));
        }
        if names.iter().any(|known| known == name) {
            out.push(bad(n, format!("«{name}» уже есть выше")));
        } else {
            names.push(name.to_string());
        }
    }
    out
}

fn validate_group(lines: &[String]) -> Vec<Problem> {
    let mut out = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() != 4 {
            out.push(bad(n, "нужно 4 поля: имя:x:gid:участники"));
            continue;
        }
        if fields[0].is_empty() {
            out.push(bad(n, "пустое имя группы"));
        }
        if fields[2].parse::<u32>().is_err() {
            out.push(bad(n, format!("gid «{}» — не число", fields[2])));
        }
    }
    out
}

fn validate_hostname(lines: &[String]) -> Vec<Problem> {
    let mut out = Vec::new();
    let used: Vec<(usize, &str)> =
        lines.iter().enumerate().map(|(i, l)| (i + 1, l.trim())).filter(|(_, l)| !l.is_empty()).collect();
    match used.as_slice() {
        [] => out.push(bad(0, "файл пуст: имя компьютера — одна строка")),
        [(n, name), rest @ ..] => {
            let ok = name.len() <= 63
                && !name.starts_with('-')
                && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
            if !ok {
                out.push(bad(*n, "только a-z, 0-9 и «-», до 63 знаков, не с «-»"));
            }
            for (n, _) in rest {
                out.push(bad(*n, "лишняя строка: имя — одна строка"));
            }
        }
    }
    out
}

/// Что значит место под кареткой.
fn describe(kind: Kind, line: &str, col: usize) -> String {
    let trimmed = line.trim();
    match kind {
        Kind::Desktop | Kind::Httpd | Kind::Update => {
            let keys = match kind {
                Kind::Desktop => DESKTOP_KEYS,
                Kind::Httpd => HTTPD_KEYS,
                _ => UPDATE_KEYS,
            };
            if trimmed.starts_with('#') || trimmed.is_empty() {
                return String::from("Комментарий: строки с «#» никто не читает");
            }
            let key = trimmed.split_once('=').map_or(trimmed, |(key, _)| key).trim();
            match keys.iter().find(|(name, _)| *name == key) {
                Some((name, about)) => format!("Ключ «{name}»: {about}"),
                None => format!("Ключ «{key}» неизвестен"),
            }
        }
        Kind::Services => {
            let (text, off) = match trimmed.strip_prefix("#off ") {
                Some(rest) => (rest, true),
                None if trimmed.starts_with('#') || trimmed.is_empty() => {
                    return String::from("Строка службы: имя путь [uid] [gid]; «#off » в начале — служба выключена");
                }
                None => (trimmed, false),
            };
            let field = field_at(line, col, |ch| ch.is_whitespace()).saturating_sub(usize::from(off));
            let what = ["имя службы (до 24 знаков)", "программа, абсолютный путь", "uid: от чьего имени", "gid: группа"]
                .get(field)
                .copied()
                .unwrap_or("лишнее поле");
            format!("Служба{}: {what} · строка «{}»", if off { " (выключена)" } else { "" }, text)
        }
        Kind::Autostart => String::from("on|off команда — что запускать при входе; off — строка остаётся, но не запускается"),
        Kind::Passwd => {
            let field = field_at(line, col, |ch| ch == ':');
            let what = ["имя", "uid", "gid", "права", "домашний каталог", "алгоритм пароля", "соль", "отпечаток пароля"]
                .get(field)
                .copied()
                .unwrap_or("лишнее поле");
            format!("Учётная запись: поле {} — {what}", field + 1)
        }
        Kind::Group => {
            let field = field_at(line, col, |ch| ch == ':');
            let what = ["имя группы", "пароль (x)", "gid", "участники через запятую"].get(field).copied().unwrap_or("лишнее поле");
            format!("Группа: поле {} — {what}", field + 1)
        }
        Kind::Hostname => String::from("Имя компьютера: a-z, 0-9 и «-»"),
        Kind::Plain => String::from("Файл без схемы: проверки нет, правьте осторожно"),
    }
}

/// Номер поля (с нуля) под знаком `col`.
/// Для пробелов поля — слова (подряд идущие пробелы — один разделитель), для
/// «:» каждый знак — граница поля.
fn field_at(line: &str, col: usize, split: impl Fn(char) -> bool) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let col = col.min(chars.len());
    if !split(' ') {
        return chars[..col].iter().filter(|ch| split(**ch)).count();
    }
    // Номер слова, начавшегося не позже каретки.
    let mut words = 0usize;
    let mut in_word = false;
    for ch in chars.iter().take(col.max(1).min(chars.len())) {
        if split(*ch) {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
    }
    // Каретка на пробеле после слова — следующее поле.
    let next_word = chars.get(col).is_some_and(|ch| !split(*ch)) && col > 0 && split(chars[col - 1]);
    (words + usize::from(next_word)).saturating_sub(1)
}

// ---------------------------------------------------------------------------
// Файлы
// ---------------------------------------------------------------------------

struct Item {
    /// Имя относительно `/etc`: `services`, `ssh/sshd.cfg`.
    name: String,
    size: u64,
    /// Только эталон: в `/etc` файла нет.
    reference: bool,
    /// Есть ли эталон (для «Вернуть эталон»).
    has_default: bool,
    problems: usize,
    errors: bool,
}

impl Item {
    fn etc_path(&self) -> String {
        format!("{ETC}/{}", self.name)
    }

    fn read_path(&self) -> String {
        if self.reference { format!("{DEFAULTS}/{}", self.name) } else { self.etc_path() }
    }

    fn base(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

fn list_dir(path: &str) -> Vec<(String, bool, u64)> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if name != "." && name != ".." {
            out.push((name, entry.kind == KIND_DIRECTORY, entry.size));
        }
    }
    close(fd);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn scan() -> Vec<Item> {
    let mut names: Vec<(String, u64)> = Vec::new();
    for (name, folder, size) in list_dir(ETC) {
        if name.ends_with(".bak") {
            continue;
        }
        if folder {
            for (inner, inner_folder, inner_size) in list_dir(&format!("{ETC}/{name}")) {
                if !inner_folder && !inner.ends_with(".bak") {
                    names.push((format!("{name}/{inner}"), inner_size));
                }
            }
        } else {
            names.push((name, size));
        }
    }
    let defaults: Vec<(String, u64)> =
        list_dir(DEFAULTS).into_iter().filter(|(_, folder, _)| !folder).map(|(name, _, size)| (name, size)).collect();
    let mut items: Vec<Item> = names
        .iter()
        .map(|(name, size)| Item {
            name: name.clone(),
            size: *size,
            reference: false,
            has_default: defaults.iter().any(|(known, _)| known == name),
            problems: 0,
            errors: false,
        })
        .collect();
    for (name, size) in &defaults {
        if !names.iter().any(|(known, _)| known == name) {
            items.push(Item { name: name.clone(), size: *size, reference: true, has_default: true, problems: 0, errors: false });
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    for item in &mut items {
        let kind = Kind::of(item.base());
        if kind == Kind::Plain {
            continue;
        }
        if let Some(text) = read_text(&item.read_path(), SCAN_LIMIT) {
            let problems = validate(kind, &split_lines(&text));
            item.problems = problems.len();
            item.errors = problems.iter().any(|problem| problem.bad);
        }
    }
    items
}

fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(|line| line.trim_end_matches('\r').to_string()).collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn read_text(path: &str, limit: usize) -> Option<String> {
    read_bytes(path, limit).map(|(bytes, _)| String::from_utf8_lossy(&bytes).to_string())
}

/// Байты файла и обрезан ли он пределом.
fn read_bytes(path: &str, limit: usize) -> Option<(Vec<u8>, bool)> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = vec![0u8; limit + 1];
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    let cut = got > limit;
    buffer.truncate(got.min(limit));
    Some((buffer, cut))
}

/// Записать файл целиком. Ответ — код ошибки, если не вышло.
fn write_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let fd = open_write(path, true, true);
    if fd < 0 {
        return Err(fd);
    }
    let mut done = 0usize;
    while done < data.len() {
        let step = write(fd, &data[done..]);
        if step <= 0 {
            close(fd);
            return Err(if step == 0 { user_abi::ERR_NO_SPACE } else { step });
        }
        done += step as usize;
    }
    close(fd);
    Ok(())
}

fn error_text(code: i64) -> String {
    let why = match code {
        user_abi::ERR_PERMISSION => "нет прав (нужен root)",
        user_abi::ERR_NOT_FOUND => "нет такого каталога",
        user_abi::ERR_NO_SPACE => "нет места",
        user_abi::ERR_UNSUPPORTED | user_abi::ERR_IO => "том не принимает запись (только чтение?)",
        _ => "запись не удалась",
    };
    format!("{why}, код {code}")
}

fn size_text(bytes: u64) -> String {
    if bytes < 1024 { format!("{bytes} Б") } else { format!("{:.1} КБ", bytes as f32 / 1024.0) }
}

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Editor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    SaveWithErrors,
    RevertDefault,
    Discard(usize),
}

struct Doc {
    /// Путь, по которому файл сохраняется: всегда в `/etc`.
    path: String,
    /// Откуда прочитан (эталон или `/etc`).
    from: String,
    kind: Kind,
    lines: Vec<String>,
    saved: Vec<String>,
    row: usize,
    col: usize,
    problems: Vec<Problem>,
    /// Файл больше предела чтения: сохранять нельзя (обрезал бы его).
    cut: bool,
}

impl Doc {
    fn modified(&self) -> bool {
        self.lines != self.saved
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |line| line.chars().count())
    }

    fn byte_at(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices().nth(col).map_or(line.len(), |(at, _)| at)
    }

    fn revalidate(&mut self) {
        self.problems = validate(self.kind, &self.lines);
    }

    fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }
}

struct Plan {
    toolbar: Rect,
    save: Rect,
    undo: Rect,
    reset: Rect,
    refresh: Rect,
    list_head: Rect,
    list: Rect,
    head: Rect,
    editor: Rect,
    info: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let mut x = frame.toolbar.x + pad;
        let mut button = |label: &str| {
            let rect = Rect::new(x, by, face.width(label) + ctx.px(46), bh);
            x = rect.right() + ctx.px(6) as i32;
            rect
        };
        let save = button("Сохранить");
        let undo = button("Отменить правки");
        let reset = button("Вернуть эталон");
        let refresh = button("Обновить");
        let body = frame.body;
        let list_w = ctx.px(LIST_W).min(body.w * 2 / 5);
        let caps_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(18);
        let list_head = Rect::new(body.x, body.y, list_w, caps_h);
        let list = Rect::new(body.x, list_head.bottom(), list_w, body.h.saturating_sub(caps_h));
        let right = Rect::new(body.x + list_w as i32, body.y, body.w.saturating_sub(list_w), body.h);
        let info_h = ctx.px(INFO_H).min(right.h / 3);
        let info = Rect::new(right.x, right.bottom() - info_h as i32, right.w, info_h);
        let head_h = u32::from(ctx.face(Role::Strong).line) + ctx.px(18);
        let head = Rect::new(right.x, right.y, right.w, head_h);
        let editor = Rect::new(right.x, head.bottom(), right.w, (info.y - head.bottom()).max(0) as u32);
        Self { toolbar: frame.toolbar, save, undo, reset, refresh, list_head, list, head, editor, info, status: frame.status }
    }
}

/// Геометрия текста в редакторе.
struct TextBox {
    line_h: u32,
    gutter: Rect,
    text: Rect,
    rows: usize,
}

impl TextBox {
    fn new(ctx: Ctx, editor: Rect, lines: usize) -> Self {
        let face = ctx.face(Role::Mono);
        let digits = format!("{}", lines.max(1)).len().max(3) as u32;
        let gutter_w = face.width("0") * digits + ctx.px(24);
        let line_h = u32::from(face.line) + ctx.px(4);
        let top = ctx.px(6);
        let gutter = Rect::new(editor.x, editor.y, gutter_w.min(editor.w), editor.h);
        let text = Rect::new(
            gutter.right() + ctx.px(10) as i32,
            editor.y + top as i32,
            editor.w.saturating_sub(gutter_w + ctx.px(22)),
            editor.h.saturating_sub(top * 2),
        );
        let rows = (text.h / line_h.max(1)).max(1) as usize;
        Self { line_h, gutter, text, rows }
    }
}

struct CfgEdit {
    items: Vec<Item>,
    selected: usize,
    list_top: Cell<usize>,
    list_rows: Cell<usize>,
    doc: Option<Doc>,
    /// Первая видимая строка и первый видимый знак текста.
    top: Cell<usize>,
    left: Cell<usize>,
    page: Cell<usize>,
    focus: Focus,
    note: Option<String>,
    confirm: Option<Confirm>,
    /// Над какой панелью указатель (для колеса): `true` — над списком.
    over_list: bool,
    root: bool,
}

impl CfgEdit {
    fn new(path: Option<&str>) -> Self {
        let mut window = Self {
            items: scan(),
            selected: 0,
            list_top: Cell::new(0),
            list_rows: Cell::new(1),
            doc: None,
            top: Cell::new(0),
            left: Cell::new(0),
            page: Cell::new(10),
            focus: Focus::List,
            note: None,
            confirm: None,
            over_list: false,
            root: user_progs::uid() == 0,
        };
        println(&format!("cfgedit: ready, {} file(s)", window.items.len()));
        match path {
            Some(path) => {
                let name = path.strip_prefix("/etc/").or_else(|| path.strip_prefix("/usr/share/defaults/etc/")).unwrap_or(path);
                let name = name.trim_start_matches('/').to_string();
                match window.items.iter().position(|item| item.name == name) {
                    Some(index) => window.open_item(index),
                    None => {
                        // Новый файл в /etc: открыть пустым.
                        window.items.push(Item { name: name.clone(), size: 0, reference: false, has_default: false, problems: 0, errors: false });
                        let index = window.items.len() - 1;
                        window.open_item(index);
                    }
                }
                window.focus = Focus::Editor;
            }
            None => {
                if !window.items.is_empty() {
                    window.open_item(0);
                }
            }
        }
        window
    }

    fn open_item(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else { return };
        self.selected = index;
        let from = item.read_path();
        let (text, cut) = match read_bytes(&from, READ_LIMIT) {
            Some((bytes, cut)) => (String::from_utf8_lossy(&bytes).to_string(), cut),
            None => (String::new(), false),
        };
        let lines = split_lines(&text);
        let kind = Kind::of(item.base());
        let mut doc = Doc {
            path: item.etc_path(),
            from: from.clone(),
            kind,
            saved: lines.clone(),
            lines,
            row: 0,
            col: 0,
            problems: Vec::new(),
            cut,
        };
        doc.revalidate();
        let count = doc.problems.len();
        if count == 0 {
            println(&format!("cfgedit: open {from}: {} line(s), valid", doc.lines.len()));
        } else {
            println(&format!("cfgedit: open {from}: {} line(s), {count} problem(s)", doc.lines.len()));
            for problem in doc.problems.iter().take(10) {
                println(&format!("cfgedit: problem {from}:{}: {}", problem.line, problem.text));
            }
        }
        self.items[index].problems = count;
        self.items[index].errors = doc.problems.iter().any(|problem| problem.bad);
        self.doc = Some(doc);
        self.top.set(0);
        self.left.set(0);
        self.confirm = None;
        self.note = cut.then(|| String::from("Файл больше 256 КБ: показано начало, сохранение выключено"));
    }

    /// Открыть файл из списка, спросив про несохранённое.
    fn open_checked(&mut self, index: usize) {
        if self.doc.as_ref().is_some_and(Doc::modified) && self.confirm != Some(Confirm::Discard(index)) {
            self.confirm = Some(Confirm::Discard(index));
            self.note = Some(String::from("Есть несохранённые правки — Enter ещё раз, чтобы открыть другой файл без сохранения"));
            return;
        }
        self.open_item(index);
        self.focus = Focus::Editor;
    }

    fn refresh(&mut self) {
        let current = self.items.get(self.selected).map(|item| item.name.clone());
        self.items = scan();
        if let Some(name) = current {
            if let Some(index) = self.items.iter().position(|item| item.name == name) {
                self.selected = index;
            }
        }
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        if let (Some(doc), Some(item)) = (self.doc.as_ref(), self.items.get_mut(self.selected)) {
            if item.etc_path() == doc.path {
                item.problems = doc.problems.len();
                item.errors = doc.problems.iter().any(|problem| problem.bad);
            }
        }
        self.note = Some(format!("Список обновлён: файлов {}", self.items.len()));
    }

    fn save(&mut self) {
        let Some(doc) = self.doc.as_mut() else { return };
        if doc.cut {
            self.note = Some(String::from("Файл показан не целиком — сохранение обрезало бы его"));
            return;
        }
        let errors = doc.problems.iter().filter(|problem| problem.bad).count();
        if errors > 0 && self.confirm != Some(Confirm::SaveWithErrors) {
            self.confirm = Some(Confirm::SaveWithErrors);
            self.note = Some(format!("Есть ошибки ({errors}) — Ctrl+S ещё раз, чтобы сохранить"));
            return;
        }
        self.confirm = None;
        let path = doc.path.clone();
        // Прежняя версия рядом — если в /etc что-то было.
        if let Some((old, _)) = read_bytes(&path, READ_LIMIT) {
            if let Err(code) = write_file(&format!("{path}.bak"), &old) {
                println(&format!("cfgedit: save FAILED {path}: {code}"));
                self.note = Some(format!("Не сохранено: копия {path}.bak не записалась — {}", error_text(code)));
                return;
            }
        }
        let text = doc.text();
        match write_file(&path, text.as_bytes()) {
            Ok(()) => {
                println(&format!("cfgedit: saved {path} ({} bytes)", text.len()));
                doc.saved = doc.lines.clone();
                doc.from = path.clone();
                self.note = Some(format!("Сохранено: {path} ({} байт), прежняя версия — {path}.bak", text.len()));
                if let Some(item) = self.items.get_mut(self.selected) {
                    item.reference = false;
                    item.size = text.len() as u64;
                }
            }
            Err(code) => {
                println(&format!("cfgedit: save FAILED {path}: {code}"));
                self.note = Some(format!("Не сохранено: {}", error_text(code)));
            }
        }
    }

    fn undo(&mut self) {
        let Some(doc) = self.doc.as_mut() else { return };
        if !doc.modified() {
            self.note = Some(String::from("Правок нет"));
            return;
        }
        doc.lines = doc.saved.clone();
        doc.row = doc.row.min(doc.lines.len().saturating_sub(1));
        doc.col = doc.col.min(doc.line_len(doc.row));
        doc.revalidate();
        self.note = Some(String::from("Правки отменены: текст как при открытии или последнем сохранении"));
        println("cfgedit: edits reverted");
    }

    fn reset_default(&mut self) {
        let Some(item) = self.items.get(self.selected) else { return };
        if !item.has_default || item.reference {
            self.note = Some(String::from("У этого файла нет правки поверх эталона"));
            return;
        }
        if self.confirm != Some(Confirm::RevertDefault) {
            self.confirm = Some(Confirm::RevertDefault);
            self.note = Some(format!("Удалить {} и вернуться к эталону? Нажмите «Вернуть эталон» ещё раз", item.etc_path()));
            return;
        }
        self.confirm = None;
        let path = item.etc_path();
        let result = remove(&path);
        if result < 0 {
            println(&format!("cfgedit: reset FAILED {path}: {result}"));
            self.note = Some(format!("Не удалось вернуть эталон: {}", error_text(result)));
            return;
        }
        println(&format!("cfgedit: {path} removed, back to the reference copy"));
        let index = self.selected;
        self.items[index].reference = true;
        self.open_item(index);
        self.note = Some(format!("{path} удалён: система снова читает эталон"));
    }

    // ── Правка текста ──────────────────────────────────────────────────────

    fn edit(&mut self, change: impl FnOnce(&mut Doc)) {
        if let Some(doc) = self.doc.as_mut() {
            change(doc);
            doc.revalidate();
            self.confirm = None;
        }
    }

    fn insert(&mut self, text: &str) {
        self.edit(|doc| {
            if doc.lines.is_empty() {
                doc.lines.push(String::new());
            }
            let at = doc.byte_at(doc.row, doc.col);
            doc.lines[doc.row].insert_str(at, text);
            doc.col += text.chars().count();
        });
    }

    fn newline(&mut self) {
        self.edit(|doc| {
            if doc.lines.is_empty() {
                doc.lines.push(String::new());
            }
            let at = doc.byte_at(doc.row, doc.col);
            let rest = doc.lines[doc.row].split_off(at);
            doc.lines.insert(doc.row + 1, rest);
            doc.row += 1;
            doc.col = 0;
        });
    }

    fn backspace(&mut self) {
        self.edit(|doc| {
            if doc.lines.is_empty() {
                return;
            }
            if doc.col > 0 {
                let start = doc.byte_at(doc.row, doc.col - 1);
                let end = doc.byte_at(doc.row, doc.col);
                doc.lines[doc.row].replace_range(start..end, "");
                doc.col -= 1;
            } else if doc.row > 0 {
                let line = doc.lines.remove(doc.row);
                doc.row -= 1;
                doc.col = doc.line_len(doc.row);
                doc.lines[doc.row].push_str(&line);
            }
        });
    }

    fn delete(&mut self) {
        self.edit(|doc| {
            if doc.lines.is_empty() {
                return;
            }
            if doc.col < doc.line_len(doc.row) {
                let start = doc.byte_at(doc.row, doc.col);
                let end = doc.byte_at(doc.row, doc.col + 1);
                doc.lines[doc.row].replace_range(start..end, "");
            } else if doc.row + 1 < doc.lines.len() {
                let line = doc.lines.remove(doc.row + 1);
                doc.lines[doc.row].push_str(&line);
            }
        });
    }

    fn move_to(&mut self, row: usize, col: usize) {
        if let Some(doc) = self.doc.as_mut() {
            doc.row = row.min(doc.lines.len().saturating_sub(1));
            doc.col = col.min(doc.line_len(doc.row));
        }
    }

    fn editor_key(&mut self, code: u32) -> bool {
        let Some(doc) = self.doc.as_ref() else { return false };
        let (row, col) = (doc.row, doc.col);
        let page = self.page.get().max(1);
        match code {
            ENTER => self.newline(),
            BACKSPACE => self.backspace(),
            WIN_KEY_DELETE | DELETE_ASCII => self.delete(),
            TAB => self.insert("    "),
            WIN_KEY_LEFT => {
                if col > 0 {
                    self.move_to(row, col - 1);
                } else if row > 0 {
                    let len = doc.line_len(row - 1);
                    self.move_to(row - 1, len);
                }
            }
            WIN_KEY_RIGHT => {
                if col < doc.line_len(row) {
                    self.move_to(row, col + 1);
                } else if row + 1 < doc.lines.len() {
                    self.move_to(row + 1, 0);
                }
            }
            WIN_KEY_UP => self.move_to(row.saturating_sub(1), col),
            WIN_KEY_DOWN => self.move_to(row + 1, col),
            WIN_KEY_PAGE_UP => self.move_to(row.saturating_sub(page), col),
            WIN_KEY_PAGE_DOWN => self.move_to(row + page, col),
            WIN_KEY_HOME => self.move_to(row, 0),
            WIN_KEY_END => self.move_to(row, usize::MAX),
            code => match char::from_u32(code) {
                Some(ch) if !ch.is_control() => {
                    let mut buffer = [0u8; 4];
                    self.insert(ch.encode_utf8(&mut buffer));
                }
                _ => return false,
            },
        }
        true
    }

    fn list_key(&mut self, code: u32) -> bool {
        let last = self.items.len().saturating_sub(1);
        match code {
            WIN_KEY_UP => self.selected = self.selected.saturating_sub(1),
            WIN_KEY_DOWN => self.selected = (self.selected + 1).min(last),
            WIN_KEY_HOME => self.selected = 0,
            WIN_KEY_END => self.selected = last,
            WIN_KEY_PAGE_UP => self.selected = self.selected.saturating_sub(self.list_rows.get().max(1)),
            WIN_KEY_PAGE_DOWN => self.selected = (self.selected + self.list_rows.get().max(1)).min(last),
            ENTER => self.open_checked(self.selected),
            _ => return false,
        }
        true
    }

    /// Первая видимая строка списка так, чтобы выбранная была видна.
    fn list_view_top(&self, visible: usize) -> usize {
        let visible = visible.max(1);
        let max_top = self.items.len().saturating_sub(visible);
        let mut top = self.list_top.get().min(max_top);
        if self.selected < top {
            top = self.selected;
        } else if self.selected >= top + visible {
            top = self.selected + 1 - visible;
        }
        top
    }

    // ── Рисование ──────────────────────────────────────────────────────────

    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        s.fill(Rect::new(plan.list_head.x, plan.list_head.y, plan.list_head.w, plan.list_head.h + plan.list.h), fill);
        let on = ctx.on(fill);
        let pad = ctx.px(16) as i32;
        paint::caps(on, s, plan.list_head.x + pad, paint::baseline(on, Role::MonoCaps, plan.list_head), "ФАЙЛЫ /etc");
        draw::vline(s, plan.list.right() - 1, plan.list_head.y, plan.list_head.h + plan.list.h, p.line.color, p.line.alpha);
        let row_h = ctx.px(LIST_ROW_H);
        let visible = ((plan.list.h / row_h.max(1)) as usize).max(1);
        let top = self.list_view_top(visible);
        self.list_top.set(top);
        self.list_rows.set(visible);
        let g = ctx.px(18);
        for (index, item) in self.items.iter().enumerate().skip(top).take(visible) {
            let at = (index - top) as u32;
            let row = Rect::new(plan.list.x + ctx.px(6) as i32, plan.list.y + (at * row_h) as i32, plan.list.w.saturating_sub(ctx.px(14)), row_h.saturating_sub(ctx.px(2)));
            let selected = index == self.selected;
            if selected {
                paint::row(on, s, row, if self.focus == Focus::List { RowState::Selected } else { RowState::Hover });
            }
            let ink_ctx = on;
            let icon = if item.reference { Fi::FileText } else { Fi::FileCode };
            iconset::draw(s, icon, row.x + ctx.px(10) as i32, row.y + (row.h - g) as i32 / 2, g, if selected && self.focus == Focus::List { p.acc_ink } else { p.ink3 }, 255);
            let tx = row.x + (g + ctx.px(20)) as i32;
            let (chip, tone) = if item.problems == 0 {
                (String::new(), Tone::Ok)
            } else if item.errors {
                (format!("{}", item.problems), Tone::Bad)
            } else {
                (format!("{}", item.problems), Tone::Warn)
            };
            let chip_w = if chip.is_empty() { 0 } else { paint::chip_width(ctx, &chip).max(ctx.px(28)) };
            let room = (row.right() - tx - chip_w as i32 - ctx.px(10) as i32).max(0) as u32;
            let name_y = row.y + ctx.px(6) as i32;
            paint::text_clipped(ink_ctx, s, Role::Body, tx, name_y, room, &item.name, p.ink);
            let meta = format!(
                "{} · {}{}",
                size_text(item.size),
                Kind::of(item.base()).title(),
                if item.reference { " · эталон" } else { "" }
            );
            let meta_y = name_y + i32::from(ctx.face(Role::Body).line) + ctx.px(2) as i32;
            paint::text_clipped(ink_ctx, s, Role::MonoSmall, tx, meta_y, room, &meta, if item.reference { p.acc } else { p.ink4 });
            if !chip.is_empty() {
                let chip_rect = Rect::new(row.right() - chip_w as i32 - ctx.px(8) as i32, row.y + (row.h as i32 - ctx.px(22) as i32) / 2, chip_w, ctx.px(22));
                paint::chip(ink_ctx, s, chip_rect, &chip, tone);
            }
        }
        if self.items.is_empty() {
            paint::text_clipped(on, s, Role::Body, plan.list.x + pad, plan.list.y + ctx.px(12) as i32, plan.list.w.saturating_sub(ctx.px(32)), "В /etc ничего нет", p.ink4);
        }
        if self.items.len() > visible {
            let track = Rect::new(plan.list.right() - ctx.px(8) as i32, plan.list.y, ctx.px(4), (visible as u32 * row_h).min(plan.list.h));
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / self.items.len()) as u32).max(ctx.px(20)).min(track.h);
            let max_top = self.items.len() - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
    }

    fn draw_editor(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let pad = ctx.px(16) as i32;
        // Шапка: путь, откуда прочитан, отметки.
        s.fill(plan.head, theme::window_bg());
        draw::hline(s, plan.head.x, plan.head.bottom() - 1, plan.head.w, p.line.color, p.line.alpha);
        let Some(doc) = self.doc.as_ref() else {
            paint::text(ctx, s, Role::Body, plan.head.x + pad, paint::baseline(ctx, Role::Body, plan.head), "Выберите файл слева", p.ink4);
            return;
        };
        let mut x = plan.head.x + pad;
        let title = if doc.modified() { format!("{} •", doc.path) } else { doc.path.clone() };
        x += paint::text(ctx, s, Role::Strong, x, paint::baseline(ctx, Role::Strong, plan.head), &title, p.ink) as i32 + ctx.px(12) as i32;
        let chip_h = ctx.px(22);
        let chip_y = plan.head.y + (plan.head.h as i32 - chip_h as i32) / 2;
        let mut chips: Vec<(String, Tone)> = Vec::new();
        if doc.from != doc.path {
            chips.push((String::from("ЭТАЛОН"), Tone::Accent));
        }
        if doc.modified() {
            chips.push((String::from("ИЗМЕНЁН"), Tone::Warn));
        }
        let errors = doc.problems.iter().filter(|problem| problem.bad).count();
        let warnings = doc.problems.len() - errors;
        if doc.kind != Kind::Plain {
            chips.push(match (errors, warnings) {
                (0, 0) => (String::from("ВЕРНО"), Tone::Ok),
                (0, w) => (format!("ЗАМЕЧАНИЙ {w}"), Tone::Warn),
                (e, _) => (format!("ОШИБОК {e}"), Tone::Bad),
            });
        }
        for (label, tone) in &chips {
            let w = paint::chip_width(ctx, label);
            if x + w as i32 > plan.head.right() - pad {
                break;
            }
            paint::chip(ctx, s, Rect::new(x, chip_y, w, chip_h), label, *tone);
            x += (w + ctx.px(8)) as i32;
        }

        // Текст.
        s.fill(plan.editor, theme::window_bg());
        let tb = TextBox::new(ctx, plan.editor, doc.lines.len());
        let gutter_fill = ctx.flat(p.panel);
        s.fill(tb.gutter, gutter_fill);
        draw::vline(s, tb.gutter.right() - 1, tb.gutter.y, tb.gutter.h, p.line.color, p.line.alpha);
        let rows = tb.rows;
        self.page.set(rows.saturating_sub(1).max(1));
        // Прокрутка следует за кареткой.
        let mut top = self.top.get().min(doc.lines.len().saturating_sub(1));
        if doc.row < top {
            top = doc.row;
        } else if doc.row >= top + rows {
            top = doc.row + 1 - rows;
        }
        self.top.set(top);
        let face = ctx.face(Role::Mono);
        let cw = face.width("0").max(1);
        let cols = (tb.text.w / cw).max(4) as usize;
        let mut left = self.left.get();
        if doc.col < left {
            left = doc.col.saturating_sub(4);
        } else if doc.col >= left + cols {
            left = doc.col + 4 - cols.min(doc.col + 4);
        }
        self.left.set(left);
        let gutter_ctx = ctx.on(gutter_fill);
        let focused = self.focus == Focus::Editor;
        for (index, line) in doc.lines.iter().enumerate().skip(top).take(rows) {
            let y = tb.text.y + ((index - top) as u32 * tb.line_h) as i32;
            let band = Rect::new(tb.text.x - ctx.px(6) as i32, y, tb.text.w + ctx.px(6), tb.line_h);
            if index == doc.row {
                s.fill(band, ctx.flat(if focused { p.sel1 } else { p.sunk }));
            }
            let problem = doc.problems.iter().filter(|problem| problem.line == index + 1).max_by_key(|problem| problem.bad);
            let number_ink = match problem {
                Some(problem) if problem.bad => p.bad_ink,
                Some(_) => p.warn,
                None if index == doc.row => p.ink2,
                None => p.ink5,
            };
            let ty = y + (tb.line_h as i32 - i32::from(face.line)) / 2;
            paint::text_right(gutter_ctx, s, Role::Mono, tb.gutter.right() - ctx.px(10) as i32, ty, &format!("{}", index + 1), number_ink);
            if problem.is_some() {
                let dot = ctx.px(6);
                draw::rounded(s, Rect::new(tb.gutter.x + ctx.px(5) as i32, y + (tb.line_h - dot) as i32 / 2, dot, dot), dot / 2, number_ink, 255);
            }
            let shown: String = line.chars().skip(left).collect();
            let comment = line.trim_start().starts_with('#');
            let ink = if comment { p.ink4 } else { p.ink };
            let line_ctx = if index == doc.row { ctx.on(ctx.flat(if focused { p.sel1 } else { p.sunk })) } else { ctx };
            paint::text_clipped(line_ctx, s, Role::Mono, tb.text.x, ty, tb.text.w, &shown, ink);
            if index == doc.row && focused {
                let before: String = line.chars().skip(left).take(doc.col.saturating_sub(left)).collect();
                let cx = tb.text.x + face.width(&before) as i32;
                if cx <= tb.text.right() {
                    s.fill(Rect::new(cx, y + ctx.px(2) as i32, ctx.px(2), tb.line_h.saturating_sub(ctx.px(4))), p.acc);
                }
            }
        }
        if doc.lines.is_empty() || (doc.lines.len() == 1 && doc.lines[0].is_empty() && !focused) {
            paint::text(ctx, s, Role::Body, tb.text.x, tb.text.y + ctx.px(2) as i32, "Файл пуст — начните печатать", p.ink5);
        }
        if doc.lines.len() > rows {
            let track = Rect::new(plan.editor.right() - ctx.px(8) as i32, tb.text.y, ctx.px(4), tb.text.h);
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * rows / doc.lines.len()) as u32).max(ctx.px(20)).min(track.h);
            let max_top = doc.lines.len() - rows;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
    }

    /// Строки панели проверки: (прямоугольник, номер строки файла).
    fn info_rows(ctx: Ctx, plan: &Plan, count: usize) -> Vec<Rect> {
        let pad = ctx.px(16);
        let line_h = u32::from(ctx.face(Role::Mono).line) + ctx.px(6);
        let title_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(16);
        let about_h = u32::from(ctx.face(Role::Body).line) + ctx.px(14);
        let room = plan.info.h.saturating_sub(title_h + about_h);
        let fit = (room / line_h.max(1)) as usize;
        (0..count.min(fit))
            .map(|index| Rect::new(plan.info.x + pad as i32, plan.info.y + (title_h + index as u32 * line_h) as i32, plan.info.w.saturating_sub(pad * 2), line_h))
            .collect()
    }

    fn draw_info(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        s.fill(plan.info, fill);
        draw::hline(s, plan.info.x, plan.info.y, plan.info.w, p.line.color, p.line.alpha);
        let on = ctx.on(fill);
        let pad = ctx.px(16) as i32;
        let title_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(16);
        let title = Rect::new(plan.info.x, plan.info.y, plan.info.w, title_h);
        let Some(doc) = self.doc.as_ref() else { return };
        paint::caps(on, s, plan.info.x + pad, paint::baseline(on, Role::MonoCaps, title), "ПРОВЕРКА");
        let summary = if doc.kind == Kind::Plain {
            String::from("файл без схемы — не проверяется")
        } else if doc.problems.is_empty() {
            format!("{}: замечаний нет", doc.kind.title())
        } else {
            format!("{}: замечаний {}", doc.kind.title(), doc.problems.len())
        };
        paint::text_right(on, s, Role::MonoSmall, plan.info.right() - pad, paint::baseline(on, Role::MonoSmall, title), &summary, p.ink4);
        let rows = Self::info_rows(ctx, plan, doc.problems.len());
        let g = ctx.px(14);
        for (rect, problem) in rows.iter().zip(&doc.problems) {
            let (icon, ink) = if problem.bad { (Fi::X, p.bad_ink) } else { (Fi::Warning, p.warn) };
            iconset::draw(s, icon, rect.x, rect.y + (rect.h - g) as i32 / 2, g, ink, 255);
            let place = if problem.line == 0 { String::from("файл") } else { format!("строка {}", problem.line) };
            let text = format!("{place}: {}", problem.text);
            paint::text_clipped(on, s, Role::Mono, rect.x + (g + ctx.px(8)) as i32, paint::baseline(on, Role::Mono, *rect), rect.w.saturating_sub(g + ctx.px(8)), &text, if problem.bad { p.ink } else { p.ink2 });
        }
        if rows.len() < doc.problems.len() {
            let y = rows.last().map_or(plan.info.y + title_h as i32, |rect| rect.bottom());
            paint::text(on, s, Role::MonoSmall, plan.info.x + pad, y, &format!("… и ещё {}", doc.problems.len() - rows.len()), p.ink4);
        }
        // Описание места под кареткой — нижней строкой.
        let about_h = u32::from(ctx.face(Role::Body).line) + ctx.px(14);
        let about = Rect::new(plan.info.x, plan.info.bottom() - about_h as i32, plan.info.w, about_h);
        draw::hline(s, about.x + pad, about.y, about.w.saturating_sub(pad as u32 * 2), p.line.color, p.line.alpha);
        let line = doc.lines.get(doc.row).map_or("", String::as_str);
        let text = describe(doc.kind, line, doc.col);
        iconset::draw(s, Fi::Info, about.x + pad, about.y + (about.h - g) as i32 / 2, g, p.acc, 255);
        paint::text_clipped(on, s, Role::Body, about.x + pad + (g + ctx.px(8)) as i32, paint::baseline(on, Role::Body, about), about.w.saturating_sub(pad as u32 * 2 + g + ctx.px(8)), &text, p.ink2);
    }
}

impl App for CfgEdit {
    /// Smallest window (24.11, WM SDK): the 290-point file list and the editor beside it.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(640, 400)
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & WIN_MOD_CTRL != 0;
        if ctrl {
            match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                Some('s') => self.save(),
                Some('z') => self.undo(),
                Some('r') => self.refresh(),
                _ => return false,
            }
            return true;
        }
        match code {
            CTRL_S => {
                self.save();
                return true;
            }
            CTRL_Z => {
                self.undo();
                return true;
            }
            F5 => {
                self.refresh();
                return true;
            }
            F6 => {
                self.focus = if self.focus == Focus::List { Focus::Editor } else { Focus::List };
                return true;
            }
            ESCAPE => {
                self.confirm = None;
                self.note = None;
                if self.focus == Focus::Editor {
                    self.focus = Focus::List;
                }
                return true;
            }
            _ => {}
        }
        // Подсказка живёт до следующей клавиши; вопрос «открыть без
        // сохранения?» — только до Enter в списке.
        self.note = None;
        if code != ENTER && matches!(self.confirm, Some(Confirm::Discard(_))) {
            self.confirm = None;
        }
        match self.focus {
            Focus::List => self.list_key(code),
            Focus::Editor => self.editor_key(code),
        }
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.save.contains(x, y) {
            self.save();
        } else if plan.undo.contains(x, y) {
            self.undo();
        } else if plan.reset.contains(x, y) {
            self.reset_default();
        } else if plan.refresh.contains(x, y) {
            self.refresh();
        } else if plan.list.contains(x, y) {
            self.focus = Focus::List;
            let index = self.list_top.get() + ((y - plan.list.y) as u32 / ctx.px(LIST_ROW_H)) as usize;
            if index < self.items.len() {
                self.selected = index;
                self.open_checked(index);
            }
        } else if plan.editor.contains(x, y) {
            self.focus = Focus::Editor;
            let Some(doc) = self.doc.as_ref() else { return true };
            let tb = TextBox::new(ctx, plan.editor, doc.lines.len());
            let row = self.top.get() + ((y - tb.text.y).max(0) as u32 / tb.line_h.max(1)) as usize;
            let row = row.min(doc.lines.len().saturating_sub(1));
            let face = ctx.face(Role::Mono);
            let left = self.left.get();
            let mut col = left;
            let mut at = tb.text.x;
            if let Some(line) = doc.lines.get(row) {
                for ch in line.chars().skip(left) {
                    let mut buffer = [0u8; 4];
                    let w = face.width(ch.encode_utf8(&mut buffer)) as i32;
                    if x < at + w / 2 {
                        break;
                    }
                    at += w;
                    col += 1;
                }
            }
            self.move_to(row, col);
        } else if plan.info.contains(x, y) {
            let Some(doc) = self.doc.as_ref() else { return false };
            let rows = Self::info_rows(ctx, &plan, doc.problems.len());
            if let Some(index) = rows.iter().position(|rect| rect.contains(x, y)) {
                let line = doc.problems[index].line;
                if line > 0 {
                    self.move_to(line - 1, 0);
                    self.focus = Focus::Editor;
                }
            }
        } else {
            return false;
        }
        true
    }

    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        self.over_list = plan.list.contains(x, y) || plan.list_head.contains(x, y);
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        if self.over_list {
            let visible = self.list_rows.get().max(1);
            let max_top = self.items.len().saturating_sub(visible);
            let top = if clicks > 0 { self.list_top.get().saturating_sub(1) } else { (self.list_top.get() + 1).min(max_top) };
            self.list_top.set(top);
            self.selected = self.selected.clamp(top, (top + visible - 1).min(self.items.len().saturating_sub(1)).max(top));
            return true;
        }
        let Some(doc) = self.doc.as_ref() else { return false };
        let step = 3usize;
        let row = if clicks > 0 { doc.row.saturating_sub(step) } else { doc.row + step };
        let col = doc.col;
        self.move_to(row, col);
        true
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Configuration Editor\x1fРедактор конфигурации"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        let doc = self.doc.as_ref();
        let item = self.items.get(self.selected);
        let can_save = doc.is_some_and(|doc| !doc.cut && (doc.modified() || doc.from != doc.path));
        tool_button(bar, s, plan.save, Fi::Check, "Сохранить", can_save);
        tool_button(bar, s, plan.undo, Fi::Undo, "Отменить правки", doc.is_some_and(Doc::modified));
        tool_button(bar, s, plan.reset, Fi::History, "Вернуть эталон", item.is_some_and(|item| item.has_default && !item.reference));
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить", true);
        self.draw_list(s, ctx, &plan);
        self.draw_editor(s, ctx, &plan);
        self.draw_info(s, ctx, &plan);
        let status = self.note.clone().unwrap_or_else(|| match doc {
            Some(doc) => format!(
                "Строк: {} · строка {}, столбец {} · {} · Ctrl+S — сохранить, Ctrl+Z — отменить правки, F6 — список/текст",
                doc.lines.len(),
                doc.row + 1,
                doc.col + 1,
                if self.root { "правка доступна" } else { "сохранение в /etc — только у root" }
            ),
            None => format!("Файлов: {} · Enter — открыть, F6 — список/текст", self.items.len()),
        });
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str, enabled: bool) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    let ink = if enabled { p.ink } else { p.ink5 };
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, if enabled { p.ink2 } else { p.ink5 }, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, ink);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения — те, что ядро передало в `_start`.
    let args = unsafe { Args::new(argc, argv) };
    let path = args.get(1).map(String::from);
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (1000, 640));
    app::run(&SPEC, &info, size, move |_, _| CfgEdit::new(path.as_deref()))
}
