//! «Архиватор 7-Zip» (24.11, Роман: 7-Zip как стандартная программа) — окно
//! над консольным `7zz` (`/bin/7zz`, 7-Zip 26.06 из исходников 7-zip.org,
//! `xtask/src/sevenzip.rs`).
//!
//! ```text
//! archiver                         пустое окно: «Открыть архив…», «Создать архив…»
//! archiver АРХИВ                   окно как 7-Zip File Manager: меню, панель, адрес, столбцы
//! archiver --type ТИП АРХИВ        «Открыть архив ▸ ТИП» (`-tТИП`: 7z, zip, *, # …)
//! archiver --extract АРХИВ         «Распаковать»: куда, затем ход и закрытие
//! archiver --extract-here АРХИВ…   «Распаковать здесь»: рядом с архивом, без вопросов
//! archiver --extract-to АРХИВ…     «Распаковать в "ИМЯ\"»: папка по имени архива
//! archiver --test АРХИВ…           «Тестировать»: итог окном
//! archiver --add ПУТЬ…             «Добавить к архиву…»: имя, формат, сжатие, пароль
//! archiver --7z ПУТЬ…              «Добавить к "ИМЯ.7z"» (имя — как у 7-Zip)
//! archiver --zip ПУТЬ…             «Добавить к "ИМЯ.zip"»
//! archiver --hash МЕТОД ПУТЬ…      «CRC SHA ▸ МЕТОД» (CRC32 CRC64 SHA1 SHA256 *): итог окном
//! ```
//!
//! Пути с пробелом приходят `%20` (как `files --properties`), длинный список
//! — файлом `@/tmp/…` (строка запуска у ядра — до 255 байт).
//!
//! # Как зовётся 7zz
//!
//! Задачей с каналом на выводе (`launch`): `-bso1 -bse1 -bsp1` — вывод,
//! ошибки и ход в один канал, ход — проценты с `\b`, разбираются по кускам.
//! Ввода нет (канал закрыт сразу): пароль 7zz не спрашивает, а падает с
//! «Wrong password» — окно тогда спрашивает его само и повторяет с `-p`.
//! Список файлов внутри архива — `-slt` (блоки `ключ = значение`); имена
//! для `x`/`d`/`a` — файлом-списком `@…` (`-scsUTF-8`): строка запуска
//! короткая, имена длинные. Отмена — `kill` задачи 7zz.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::filedlg::Line;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::iconset::{self, Fi};
use mini_ui::kit::{self, Scroll, ScrollBar, Table};
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_abi::{Stat, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use user_progs::app::{self, App, Spec};
use user_progs::filedlg::{Dialog, Filter, Options, Outcome, home_dir};
use user_progs::{
    Args, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_UP, close, kill,
    launch, mkdir, monotonic_ms, open_write, pipe, println, read, spawn, stat, wait, write,
};

const SPEC: Spec = Spec { name: "archiver", title: "7-Zip", period_ms: 5_000 };

/// Консольный 7-Zip.
const SEVENZIP: &str = "/bin/7zz";

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const TAB: u32 = '\t' as u32;
const SPACE: u32 = ' ' as u32;
const DOUBLE_CLICK_MS: u64 = 500;
/// Самая длинная строка запуска у ядра.
const LINE_LIMIT: usize = 255;
/// Сколько вывода 7zz держать (список большого архива — мегабайты).
const OUTPUT_LIMIT: usize = 32 << 20;

// ---------------------------------------------------------------------------
// Содержимое архива
// ---------------------------------------------------------------------------

/// Запись архива из `7zz l -slt`.
#[derive(Clone, Default)]
struct Item {
    /// Путь внутри архива, через `/`.
    path: String,
    folder: bool,
    size: u64,
    packed: u64,
    /// «24.11.2026 10:00»; пусто — неизвестно.
    modified: String,
    encrypted: bool,
    /// The columns of 7-Zip File Manager (25.11): `Attributes`, `CRC`,
    /// `Method`, `Block` of `-slt`.
    attributes: String,
    crc: Option<u32>,
    method: String,
    block: String,
}

/// Сведения об архиве целиком.
#[derive(Default)]
struct Info {
    kind: String,
    method: String,
    physical: u64,
    solid: bool,
    blocks: String,
    headers: u64,
    /// The banner line of 7zz («7-Zip (z) 26.06 …») — «О программе».
    banner: String,
}

/// Строка списка текущей папки архива.
struct Row {
    name: String,
    folder: bool,
    size: u64,
    packed: u64,
    modified: String,
    encrypted: bool,
    attributes: String,
    /// A folder: the sum of the CRCs inside (as 7-Zip shows it).
    crc: Option<u32>,
    method: String,
    block: String,
    /// Folders and files inside a folder row (all levels).
    folders: u32,
    files: u32,
}

/// Columns of the list, as in 7-Zip File Manager: title, width in layout
/// points (the first stretches), numbers on the right.
const COLUMNS: [(&str, u32, bool); 10] = [
    ("Имя", 0, false),
    ("Размер", 120, true),
    ("Сжатый", 120, true),
    ("Изменён", 140, false),
    ("Атрибуты", 84, false),
    ("CRC", 92, true),
    ("Зашифрован", 104, false),
    ("Метод", 120, false),
    ("Блок", 60, true),
    ("Папки", 70, true),
];

/// Bytes with groups of three, as 7-Zip writes them: «354 550 709».
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

impl Row {
    /// Text of column `column`.
    fn cell(&self, column: usize) -> String {
        match column {
            0 => self.name.clone(),
            1 => if self.folder && self.size == 0 && self.files == 0 { String::new() } else { grouped(self.size) },
            2 => if self.packed == 0 && self.folder { String::new() } else { grouped(self.packed) },
            3 => self.modified.clone(),
            4 => self.attributes.clone(),
            5 => self.crc.map_or_else(String::new, |crc| format!("{crc:08X}")),
            6 => String::from(if self.encrypted { "+" } else { "-" }),
            7 => self.method.clone(),
            8 => self.block.clone(),
            _ => if self.folder { self.folders.to_string() } else { String::new() },
        }
    }
}

/// Order rows by column `column` (folders first, as 7-Zip does).
fn sort_rows(rows: &mut [Row], column: usize, descending: bool) {
    rows.sort_by(|a, b| {
        let key = match column {
            1 => a.size.cmp(&b.size),
            2 => a.packed.cmp(&b.packed),
            3 => date_key(&a.modified).cmp(&date_key(&b.modified)),
            4 => a.attributes.cmp(&b.attributes),
            5 => a.crc.cmp(&b.crc),
            6 => a.encrypted.cmp(&b.encrypted),
            7 => a.method.cmp(&b.method),
            8 => a.block.len().cmp(&b.block.len()).then_with(|| a.block.cmp(&b.block)),
            9 => a.folders.cmp(&b.folders),
            _ => core::cmp::Ordering::Equal,
        };
        let key = key.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        b.folder.cmp(&a.folder).then(if descending { key.reverse() } else { key })
    });
}

/// «24.11.2026 10:00» → «2026.11.24 10:00» for ordering.
fn date_key(text: &str) -> String {
    let (date, time) = text.split_once(' ').unwrap_or((text, ""));
    let parts: Vec<&str> = date.split('.').collect();
    if parts.len() == 3 { format!("{}.{}.{} {time}", parts[2], parts[1], parts[0]) } else { text.to_string() }
}

/// Разобрать вывод `7zz l -slt`.
fn parse_listing(text: &str) -> (Info, Vec<Item>) {
    let mut info = Info::default();
    let mut items = Vec::new();
    let mut in_items = false;
    let mut current: Option<Item> = None;
    let mut seen_archive = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if info.banner.is_empty() && line.trim_start().starts_with("7-Zip") {
            info.banner = line.trim().to_string();
        }
        if line.starts_with("----------") {
            in_items = true;
            continue;
        }
        if line.is_empty() {
            if let Some(item) = current.take() {
                if !item.path.is_empty() {
                    items.push(item);
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once(" = ") else { continue };
        if !in_items {
            match key {
                "Path" => seen_archive = true,
                "Type" if seen_archive => info.kind = value.to_string(),
                "Method" if seen_archive => info.method = value.to_string(),
                "Physical Size" => info.physical = value.trim().parse().unwrap_or(0),
                "Solid" => info.solid = value.trim() == "+",
                "Blocks" => info.blocks = value.trim().to_string(),
                "Headers Size" => info.headers = value.trim().parse().unwrap_or(0),
                _ => {}
            }
            continue;
        }
        let item = current.get_or_insert_with(Item::default);
        match key {
            "Path" => item.path = value.replace('\\', "/").trim_matches('/').to_string(),
            "Folder" => item.folder = value.trim() == "+",
            "Size" => item.size = value.trim().parse().unwrap_or(0),
            "Packed Size" => item.packed = value.trim().parse().unwrap_or(0),
            "Modified" => item.modified = short_date(value),
            "Attributes" => {
                if value.starts_with('D') {
                    item.folder = true;
                }
                // «D_ drwxr-xr-x» / «A_ -rw-r--r--»: the Windows letters.
                item.attributes = value.split_whitespace().next().unwrap_or("").trim_end_matches('_').to_string();
            }
            "Encrypted" => item.encrypted = value.trim() == "+",
            "CRC" => item.crc = u32::from_str_radix(value.trim(), 16).ok(),
            "Method" => item.method = value.trim().to_string(),
            "Block" => item.block = value.trim().to_string(),
            _ => {}
        }
    }
    if let Some(item) = current.take() {
        if !item.path.is_empty() {
            items.push(item);
        }
    }
    (info, items)
}

/// «2026-11-24 10:00:00.1234567» → «24.11.2026 10:00».
fn short_date(value: &str) -> String {
    let value = value.trim();
    // Нулевое время (1970) — «неизвестно»: архивы, собранные здесь же, его
    // и несут (у `stat` ядра нет времени изменения).
    if value.starts_with("1970-01-01 00:00:00") {
        return String::new();
    }
    let (date, time) = value.split_once(' ').unwrap_or((value, ""));
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return value.to_string();
    }
    let hm: String = time.chars().take(5).collect();
    format!("{}.{}.{} {hm}", parts[2], parts[1], parts[0])
}

/// Строки папки `dir` архива (`""` — корень): папки — и названные, и
/// подразумеваемые путями файлов (zip и tar часто не хранят папок).
fn rows_of(items: &[Item], dir: &str) -> Vec<Row> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut rows: Vec<Row> = Vec::new();
    for item in items {
        let Some(rest) = item.path.strip_prefix(prefix.as_str()) else { continue };
        if rest.is_empty() {
            continue;
        }
        let (name, deeper) = match rest.split_once('/') {
            Some((head, _)) => (head, true),
            None => (rest, false),
        };
        let folder = deeper || item.folder;
        let at = match rows.iter().position(|row| row.name == name && row.folder == folder) {
            Some(at) => at,
            None => {
                rows.push(Row {
                    name: name.to_string(),
                    folder,
                    size: 0,
                    packed: 0,
                    modified: String::new(),
                    encrypted: false,
                    attributes: String::from(if folder { "D" } else { "" }),
                    crc: None,
                    method: String::new(),
                    block: String::new(),
                    folders: 0,
                    files: 0,
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[at];
        row.encrypted |= item.encrypted;
        if deeper {
            // Something inside a folder row: count it, sum sizes and CRCs.
            if item.folder {
                row.folders += 1;
            } else {
                row.files += 1;
                row.size += item.size;
                row.packed += item.packed;
                if let Some(crc) = item.crc {
                    row.crc = Some(row.crc.unwrap_or(0).wrapping_add(crc));
                }
                if row.method.is_empty() {
                    row.method = item.method.clone();
                }
            }
        } else {
            // The entry itself.
            if !item.modified.is_empty() {
                row.modified = item.modified.clone();
            }
            if !item.attributes.is_empty() {
                row.attributes = item.attributes.clone();
            }
            if !item.folder {
                row.size = item.size;
                row.packed = item.packed;
                row.crc = item.crc;
                row.method = item.method.clone();
                row.block = item.block.clone();
            }
        }
    }
    sort_rows(&mut rows, 0, false);
    rows
}

// ---------------------------------------------------------------------------
// Задача 7zz
// ---------------------------------------------------------------------------

/// Что делает запущенный 7zz.
#[derive(Clone, PartialEq, Eq)]
enum JobKind {
    List,
    /// Извлечь в папку; `open` — открыть потом этот файл (двойной щелчок по
    /// файлу внутри архива), `show` — открыть папку в «Файлах».
    /// `then_delete` — «Переместить» (25.11): these names leave the archive
    /// after they are out.
    Extract { dest: String, open: Option<String>, show: bool, then_delete: Vec<String> },
    Test,
    Add { archive: String },
    Delete,
    /// «CRC SHA» (25.11): `7zz h -scrc<method>` of files on disk.
    Hash { method: String, what: String },
}

/// What the action window does with the archives after the first
/// (`--extract-here`, `--extract-to`, `--test` take several).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Queue {
    Here,
    To,
    Test,
}

struct Job {
    kind: JobKind,
    task: i64,
    out: i64,
    output: Vec<u8>,
    /// Начало ещё не разобранного хода.
    scanned: usize,
    percent: u32,
    current: String,
    started: u64,
    /// Строка запуска без пароля — для повтора с паролем.
    line: String,
    /// Архив этой задачи (`--extract-here` идёт по нескольким).
    archive: String,
}

/// Кавычки вокруг слова строки запуска (ядро снимает их, `launch_words`).
fn quoted(word: &str) -> String {
    format!("\"{word}\"")
}

/// Файл-список имён для 7zz в `/tmp`.
fn list_file(names: &[String]) -> Option<String> {
    let _ = mkdir("/tmp", 0o1777);
    let path = format!("/tmp/.archiver-{}-{}.lst", user_progs::pid(), monotonic_ms());
    let fd = open_write(&path, true, true);
    if fd < 0 {
        return None;
    }
    let mut text = String::new();
    for name in names {
        text.push_str(name);
        text.push('\n');
    }
    let ok = write(fd, text.as_bytes()) == text.len() as i64;
    close(fd);
    ok.then_some(path)
}

impl Job {
    /// Запустить `7zz <args>` (пароль — отдельно, в журнал не идёт).
    fn start(kind: JobKind, archive: &str, args: &str, password: Option<&str>) -> Result<Self, String> {
        let base = format!("{SEVENZIP} {args}");
        let line = match password {
            Some(word) => format!("{SEVENZIP} {} {args}", quoted(&format!("-p{word}"))),
            None => base.clone(),
        };
        if line.len() > LINE_LIMIT {
            return Err(String::from("Слишком длинный путь для 7-Zip (строка запуска длиннее 255 байт)."));
        }
        let (in_read, in_write) = pipe().map_err(|code| format!("нет канала: {code}"))?;
        let (out_read, out_write) = match pipe() {
            Ok(pair) => pair,
            Err(code) => {
                close(in_read);
                close(in_write);
                return Err(format!("нет канала: {code}"));
            }
        };
        let task = launch(&line, None, in_read, out_write);
        close(in_read);
        close(out_write);
        // Ввода у 7zz нет: вопрос о пароле читает конец файла и сдаётся.
        close(in_write);
        if task < 0 {
            close(out_read);
            return Err(format!("{SEVENZIP} не запускается (код {task}) — нет 7-Zip в системе?"));
        }
        println(&format!("archiver: running '{base}'{} as #{task}", if password.is_some() { " with a password" } else { "" }));
        Ok(Self { kind, task, out: out_read, output: Vec::new(), scanned: 0, percent: 0, current: String::new(), started: monotonic_ms(), line: base, archive: archive.to_string() })
    }

    /// Забрать вывод. `Some(код)` — 7zz закончил.
    fn poll(&mut self) -> Option<i64> {
        let mut buffer = [0u8; 8192];
        loop {
            let got = read(self.out, &mut buffer);
            if got > 0 {
                if self.output.len() < OUTPUT_LIMIT {
                    self.output.extend_from_slice(&buffer[..got as usize]);
                }
                continue;
            }
            if got == 0 {
                close(self.out);
                self.out = -1;
                self.scan_progress();
                return Some(wait(self.task));
            }
            break;
        }
        self.scan_progress();
        None
    }

    /// Ход: куски вывода между `\b`, `\r`, `\n` вида «  37% 12 - имя».
    fn scan_progress(&mut self) {
        let end = self.output.iter().rposition(|byte| matches!(byte, b'\x08' | b'\r' | b'\n')).map_or(self.scanned, |at| at + 1);
        if end <= self.scanned {
            return;
        }
        let text = String::from_utf8_lossy(&self.output[self.scanned..end]).into_owned();
        self.scanned = end;
        for piece in text.split(['\x08', '\r', '\n']) {
            let piece = piece.trim();
            let Some((number, rest)) = piece.split_once('%') else { continue };
            let Ok(percent) = number.trim().parse::<u32>() else { continue };
            self.percent = percent.min(100);
            let rest = rest.trim();
            // «12 - имя» (извлечение), «12 + имя» (добавление), «U имя».
            let name = rest.split_once(" - ").or_else(|| rest.split_once(" + ")).map_or("", |(_, name)| name);
            if !name.is_empty() {
                self.current = name.to_string();
            }
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    fn cancel(&mut self) {
        if self.task >= 0 {
            let _ = kill(self.task as u32);
            println(&format!("archiver: cancelled #{}", self.task));
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.out >= 0 {
            close(self.out);
        }
    }
}

/// Строки ошибок 7zz — для человека, без заставки.
fn error_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut take = false;
    for line in text.lines().map(|line| line.trim_matches(['\r', '\x08', ' '])) {
        if line.starts_with("ERROR") || line.starts_with("Command Line Error") || line.contains("Wrong password") || line.starts_with("Can not open") || line.contains("Cannot open") {
            take = true;
        }
        if take && !line.is_empty() && !out.contains(&line) {
            out.push(line);
        }
        if out.len() >= 4 {
            break;
        }
    }
    if out.is_empty() {
        out = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.contains('%')).rev().take(2).collect();
        out.reverse();
    }
    out.join(" ").chars().take(300).collect()
}

/// 7zz сказал, что нужен (или не тот) пароль.
fn wants_password(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("wrong password") || lower.contains("enter password") || lower.contains("can not open encrypted archive") || (lower.contains("encrypted") && lower.contains("data error"))
}

// ---------------------------------------------------------------------------
// Окна-формы
// ---------------------------------------------------------------------------

/// Форматы «Добавить в архив»: подпись, ключ `-t`, расширение, есть ли
/// уровни сжатия, умеет ли пароль, берёт ли несколько файлов и папки.
const FORMATS: [(&str, &str, &str, bool, bool, bool); 6] = [
    ("7z", "7z", "7z", true, true, true),
    ("zip", "zip", "zip", true, true, true),
    ("tar", "tar", "tar", false, false, true),
    ("gzip", "gzip", "gz", true, false, false),
    ("bzip2", "bzip2", "bz2", true, false, false),
    ("xz", "xz", "xz", true, false, false),
];

/// Уровни сжатия: подпись и `-mx`.
const LEVELS: [(&str, u32); 5] = [("Без сжатия", 0), ("Быстрый", 1), ("Обычный", 5), ("Максимальный", 7), ("Ультра", 9)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddFocus {
    Name,
    Format,
    Level,
    Password,
}

struct AddForm {
    sources: Vec<String>,
    name: Line,
    format: usize,
    level: usize,
    password: String,
    focus: AddFocus,
    message: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtractFocus {
    Dest,
    Show,
}

struct ExtractForm {
    archive: String,
    dest: Line,
    show: bool,
    focus: ExtractFocus,
    message: Option<String>,
}

/// Вопрос о пароле: строка и для чего он (повторить задачу).
struct Ask {
    text: String,
    retry: JobKind,
    line: String,
    archive: String,
}

/// Чем занято окно.
enum Mode {
    /// Архив открыт (или пусто — «Открыть архив…»).
    Browse,
    Add(AddForm),
    Extract(ExtractForm),
}

/// Что ждём от окна выбора файла.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    OpenArchive,
    ExtractTo,
    AddFiles,
    SaveArchive,
    /// «Копировать» / «Переместить» of the toolbar (25.11): where to.
    CopyTo,
    MoveTo,
}

/// Вопрос посреди окна.
enum Overlay {
    None,
    Password(Line, Ask),
    /// Сообщение: заголовок, текст, ошибка ли.
    Message(String, String, bool),
    /// Удалить выбранное из архива? (0 — «Удалить», 1 — «Отмена».)
    Delete(usize),
    /// A table of facts (25.11): «Информация» (archive properties), «CRC
    /// SHA» results, «О программе». `copy` — a «Копировать» button.
    Facts { title: String, rows: Vec<(String, String)>, copy: bool },
}

/// Menu bar of the window (25.11, as 7-Zip File Manager).
const MENUS: [&str; 6] = ["Файл", "Правка", "Вид", "Избранное", "Сервис", "Справка"];

/// Toolbar of the window, in the order and with the words of 7-Zip.
const TOOLS: [&str; 7] = ["Добавить", "Извлечь", "Тестировать", "Копировать", "Переместить", "Удалить", "Информация"];

/// What a menu item or a toolbar button does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    Open,
    OpenArchive,
    Add,
    Extract,
    Test,
    CopyTo,
    MoveTo,
    Delete,
    Info,
    CloseArchive,
    Exit,
    CreateArchive,
    SelectAll,
    SelectNone,
    Invert,
    Sort(u8),
    Up,
    Refresh,
    FavAdd,
    Fav(u8),
    FavClear,
    /// Checksum of the archive file (index in [`HASHES`]).
    Hash(u8),
    About,
}

/// «CRC SHA» methods: menu words and the `-scrc` key.
const HASHES: [(&str, &str); 5] = [("CRC-32", "CRC32"), ("CRC-64", "CRC64"), ("SHA-1", "SHA1"), ("SHA-256", "SHA256"), ("*", "*")];

const TOOL_CMDS: [Cmd; 7] = [Cmd::Add, Cmd::Extract, Cmd::Test, Cmd::CopyTo, Cmd::MoveTo, Cmd::Delete, Cmd::Info];

/// An item of a menu of the bar.
struct BarItem {
    label: String,
    keys: Option<&'static str>,
    cmd: Cmd,
    group: u8,
    enabled: bool,
}

/// Favourites: `archiver/favorites/<n>` = `archive\tfolder inside` (user scope).
const FAVORITES: usize = 10;

fn favorites() -> Vec<(String, String)> {
    (0..FAVORITES)
        .filter_map(|index| user_progs::reg_get_in("user", &format!("archiver/favorites/{index}")))
        .filter_map(|value| value.split_once('\t').map(|(archive, dir)| (archive.to_string(), dir.to_string())))
        .filter(|(archive, _)| !archive.is_empty())
        .collect()
}

struct Archiver {
    mode: Mode,
    /// Открытый архив (пусто — нет).
    archive: String,
    info: Info,
    items: Vec<Item>,
    /// Папка внутри архива.
    dir: String,
    rows: Vec<Row>,
    selected: usize,
    picked: Vec<bool>,
    scroll: Scroll,
    last_click: (u64, usize),
    job: Option<Job>,
    overlay: Overlay,
    dialog: Option<(Dialog, Pick)>,
    note: Option<String>,
    password: Option<String>,
    /// После задачи — закрыть окно (`--extract-here`, `--zip`, формы).
    quit_after: bool,
    /// Очередь `--extract-here`: архивы, которые ещё ждут.
    queue: Vec<String>,
    queue_kind: Queue,
    /// Results of `--test` over several archives.
    tested: Vec<(String, bool)>,
    done: bool,
    /// Sorted by column `.0`, descending `.1` (25.11).
    sort: (usize, bool),
    /// Open menu of the bar and its lit item.
    bar: Option<(usize, usize)>,
    /// Toolbar button under the pointer.
    hot: Option<usize>,
    /// `-t<type>` of «Открыть архив ▸» (25.11): empty — 7-Zip guesses.
    type_arg: String,
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

/// The window of 7-Zip File Manager (25.11): menu bar, toolbar of big
/// buttons, address row with «вверх», the list, the status line.
struct Plan {
    menubar: Rect,
    heads: [Rect; 6],
    toolbar: Rect,
    buttons: [Rect; 7],
    up: Rect,
    address: Rect,
    table: Table,
    columns: Vec<Rect>,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let pad = ctx.px(6) as i32;
        let menubar = Rect::new(area.x, area.y, area.w, ctx.px(30).min(area.h));
        let face = ctx.face(Role::Body);
        let mut heads = [Rect::EMPTY; 6];
        let mut x = menubar.x + pad;
        for (index, label) in MENUS.iter().enumerate() {
            let w = face.width(label) + ctx.px(20);
            heads[index] = Rect::new(x, menubar.y + ctx.pxi(3), w, menubar.h.saturating_sub(ctx.px(6)));
            x += w as i32;
        }
        let toolbar = Rect::new(area.x, menubar.bottom(), area.w, ctx.px(64));
        let small = ctx.face(Role::Caption);
        let mut buttons = [Rect::EMPTY; 7];
        let mut x = toolbar.x + pad;
        for (index, label) in TOOLS.iter().enumerate() {
            let w = (small.width(label) + ctx.px(20)).max(ctx.px(76));
            buttons[index] = Rect::new(x, toolbar.y + ctx.pxi(4), w, toolbar.h.saturating_sub(ctx.px(8)));
            x += w as i32 + ctx.pxi(2);
        }
        let row = Rect::new(area.x, toolbar.bottom(), area.w, ctx.px(38));
        let up = Rect::new(row.x + pad + ctx.pxi(2), row.y + ctx.pxi(5), ctx.px(28), ctx.px(28));
        let address = Rect::new(up.right() + ctx.pxi(6), up.y, (row.right() - pad - up.right() - ctx.pxi(6)).max(0) as u32, ctx.px(28));
        let status_h = ctx.px(theme::STATUS_H);
        let status = Rect::new(area.x, (area.bottom() - status_h as i32).max(row.bottom()), area.w, status_h);
        let body = Rect::new(area.x, row.bottom(), area.w, (status.y - row.bottom()).max(0) as u32);
        let table = Table::new(ctx, body);
        let widths: Vec<u32> = COLUMNS.iter().map(|(_, w, _)| ctx.px(*w)).collect();
        let columns = table.columns(ctx, &widths, ctx.px(200));
        Self { menubar, heads, toolbar, buttons, up, address, table, columns, status }
    }

    fn step(&self) -> u32 {
        self.table.row_h
    }

    /// Column under `x` in the header.
    fn column_at(&self, x: i32) -> Option<usize> {
        let shown: Vec<(usize, Rect)> = self.columns.iter().copied().enumerate().filter(|(_, rect)| rect.w > 0).collect();
        for (at, (index, rect)) in shown.iter().enumerate() {
            let left = if at == 0 { self.table.header.x } else { rect.x - (rect.x - shown[at - 1].1.right()).max(0) };
            let right = shown.get(at + 1).map_or(self.table.header.right(), |(_, next)| next.x);
            if x >= left && x < right {
                return Some(*index);
            }
        }
        None
    }
}

/// Раскладка форм («Добавить», «Извлечь»): поля одно под другим и кнопки.
struct FormPlan {
    head: Rect,
    fields: Vec<Rect>,
    browse: Rect,
    message: Rect,
    ok: Rect,
    cancel: Rect,
}

impl FormPlan {
    fn new(ctx: Ctx, area: Rect, rows: usize) -> Self {
        let pad = ctx.px(22) as i32;
        let head = Rect::new(area.x + pad, area.y + ctx.px(18) as i32, area.w.saturating_sub(pad as u32 * 2), ctx.px(44));
        let label = i32::from(ctx.face(Role::Caption).line) + ctx.px(6) as i32;
        let fh = ctx.px(34);
        let mut y = head.bottom() + ctx.px(18) as i32;
        let mut fields = Vec::new();
        let browse_w = ctx.px(96);
        for index in 0..rows {
            let w = if index == 0 { area.w.saturating_sub(pad as u32 * 2 + browse_w + ctx.px(8)) } else { area.w.saturating_sub(pad as u32 * 2) };
            fields.push(Rect::new(area.x + pad, y + label, w, fh));
            y += label + fh as i32 + ctx.px(12) as i32;
        }
        let browse = fields.first().map_or(Rect::EMPTY, |first| Rect::new(first.right() + ctx.px(8) as i32, first.y, browse_w, fh));
        let message = Rect::new(area.x + pad, y, area.w.saturating_sub(pad as u32 * 2), ctx.px(24));
        let bh = ctx.px(34);
        let bw = ctx.px(120);
        let by = area.bottom() - pad - bh as i32;
        let cancel = Rect::new(area.right() - pad - bw as i32, by, bw, bh);
        let ok = Rect::new(cancel.x - ctx.px(10) as i32 - bw as i32, by, bw, bh);
        Self { head, fields, browse, message, ok, cancel }
    }
}

/// Карточка хода и вопросов посреди окна.
fn card_rect(ctx: Ctx, area: Rect, h: u32) -> Rect {
    let w = ctx.px(460).min(area.w.saturating_sub(ctx.px(24)));
    Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h)
}

/// Кнопки карточки: `[действие, отмена]` у правого нижнего угла.
fn card_buttons(ctx: Ctx, card: Rect) -> [Rect; 2] {
    let pad = ctx.px(20) as i32;
    let bh = ctx.px(32);
    let bw = ctx.px(110);
    let by = card.bottom() - pad - bh as i32;
    let cancel = Rect::new(card.right() - pad - bw as i32, by, bw, bh);
    let ok = Rect::new(cancel.x - ctx.px(8) as i32 - bw as i32, by, bw, bh);
    [ok, cancel]
}

fn progress_card(ctx: Ctx, area: Rect) -> Rect {
    card_rect(ctx, area, ctx.px(190))
}

fn ask_card(ctx: Ctx, area: Rect) -> Rect {
    card_rect(ctx, area, ctx.px(200))
}

// ---------------------------------------------------------------------------
// Поведение
// ---------------------------------------------------------------------------

fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => String::from("/"),
        Some((dir, _)) => dir.to_string(),
        None => String::from("/"),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// Имя без расширений архива: «отчёт.tar.gz» → «отчёт».
fn stem(name: &str) -> String {
    let mut stem = name.to_string();
    for _ in 0..2 {
        match stem.rsplit_once('.') {
            Some((head, ext)) if !head.is_empty() && sysconf::filetypes::is_archive(&format!("x.{ext}")) => stem = head.to_string(),
            _ => break,
        }
    }
    stem
}

fn exists(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0
}

fn is_dir(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0 && info.kind == user_abi::KIND_DIRECTORY
}

/// Свободное имя: «имя», «имя (2)», … (`ext` с точкой или пусто).
fn free_name(dir: &str, base: &str, ext: &str) -> String {
    let first = join(dir, &format!("{base}{ext}"));
    if !exists(&first) {
        return first;
    }
    for n in 2..1000 {
        let candidate = join(dir, &format!("{base} ({n}){ext}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    first
}

fn size_text(bytes: u64) -> String {
    kit::size_text(bytes)
}

impl Archiver {
    fn new() -> Self {
        Self {
            mode: Mode::Browse,
            archive: String::new(),
            info: Info::default(),
            items: Vec::new(),
            dir: String::new(),
            rows: Vec::new(),
            selected: 0,
            picked: Vec::new(),
            scroll: Scroll::new(),
            last_click: (0, usize::MAX),
            job: None,
            overlay: Overlay::None,
            dialog: None,
            note: None,
            password: None,
            quit_after: false,
            queue: Vec::new(),
            queue_kind: Queue::Here,
            tested: Vec::new(),
            done: false,
            sort: (0, false),
            bar: None,
            hot: None,
            type_arg: String::new(),
        }
    }

    // ---- Архив и папки ------------------------------------------------------

    fn open_archive(&mut self, path: &str) {
        self.archive = path.to_string();
        self.password = None;
        self.items.clear();
        self.rows.clear();
        self.dir.clear();
        self.info = Info::default();
        self.list();
    }

    fn list(&mut self) {
        let args = format!("l -slt -bso1 -bse1 -sccUTF-8 {}", quoted(&self.archive));
        let password = self.password.clone();
        self.run(JobKind::List, &self.archive.clone(), &args, password.as_deref());
    }

    fn set_dir(&mut self, dir: &str) {
        self.dir = dir.to_string();
        self.rows = rows_of(&self.items, &self.dir);
        sort_rows(&mut self.rows, self.sort.0, self.sort.1);
        self.selected = 0;
        self.picked = alloc::vec![false; self.rows.len()];
        self.scroll.reset();
        println(&format!("archiver: folder '/{}' has {} entries", self.dir, self.rows.len()));
    }

    fn go_up(&mut self) -> bool {
        if self.dir.is_empty() {
            return false;
        }
        let from = file_name(&self.dir).to_string();
        let up = self.dir.rsplit_once('/').map_or(String::new(), |(head, _)| head.to_string());
        self.set_dir(&up);
        if let Some(at) = self.rows.iter().position(|row| row.folder && row.name == from) {
            self.selected = at;
            self.scroll.follow(at);
        }
        true
    }

    fn inner_path(&self, row: &Row) -> String {
        if self.dir.is_empty() { row.name.clone() } else { format!("{}/{}", self.dir, row.name) }
    }

    /// Отмеченные строки, или выбранная, если не отмечено ничего.
    fn chosen(&self) -> Vec<usize> {
        let picked: Vec<usize> = (0..self.rows.len()).filter(|index| self.picked.get(*index).copied().unwrap_or(false)).collect();
        if picked.is_empty() && self.selected < self.rows.len() { alloc::vec![self.selected] } else { picked }
    }

    fn open_selected(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.folder {
            let dir = self.inner_path(row);
            self.set_dir(&dir);
            return;
        }
        // Файл — извлечь во временную папку и открыть его программой.
        let inner = self.inner_path(row);
        let dest = format!("/tmp/archiver-{}-{}", user_progs::pid(), monotonic_ms());
        let _ = mkdir("/tmp", 0o1777);
        let _ = mkdir(&dest, 0o700);
        let target = join(&dest, &inner);
        self.extract_items(&dest, alloc::vec![inner], Some(target), false);
    }

    // ---- Задачи ---------------------------------------------------------------

    fn run(&mut self, kind: JobKind, archive: &str, args: &str, password: Option<&str>) {
        if self.job.is_some() {
            return;
        }
        // «Открыть архив ▸ тип» (25.11): the type goes to every command on
        // this archive (not to «Добавить», which names its own, nor to `h`).
        let typed;
        let args = if !self.type_arg.is_empty() && archive == self.archive && !args.contains(" -t") && !matches!(kind, JobKind::Hash { .. }) {
            typed = match args.split_once(' ') {
                Some((command, rest)) => format!("{command} {} {rest}", self.type_arg),
                None => args.to_string(),
            };
            typed.as_str()
        } else {
            args
        };
        match Job::start(kind, archive, args, password) {
            Ok(job) => self.job = Some(job),
            Err(text) => {
                println(&format!("archiver: cannot run 7zz: {text}"));
                self.overlay = Overlay::Message(String::from("Не удалось запустить 7-Zip"), text, true);
            }
        }
    }

    /// Извлечь `names` (пусто — всё) архива в `dest`.
    fn extract_items(&mut self, dest: &str, names: Vec<String>, open: Option<String>, show: bool) {
        self.extract_then(dest, names, open, show, Vec::new());
    }

    /// Extract `names` (empty — all) to `dest`; then delete `then_delete`
    /// from the archive («Переместить», 25.11).
    fn extract_then(&mut self, dest: &str, names: Vec<String>, open: Option<String>, show: bool, then_delete: Vec<String>) {
        let mut args = format!("x -y -aoa -bso1 -bse1 -bsp1 -sccUTF-8 {} {}", quoted(&format!("-o{dest}")), quoted(&self.archive));
        if !names.is_empty() {
            match list_file(&names) {
                Some(list) => args.push_str(&format!(" -scsUTF-8 {}", quoted(&format!("@{list}")))),
                None => {
                    self.overlay = Overlay::Message(String::from("Не удалось извлечь"), String::from("Не записать список файлов в /tmp."), true);
                    return;
                }
            }
        }
        let password = self.password.clone();
        let archive = self.archive.clone();
        self.run(JobKind::Extract { dest: dest.to_string(), open, show, then_delete }, &archive, &args, password.as_deref());
    }

    fn extract_here(&mut self, archive: &str) {
        let dir = parent(archive);
        // Как «Извлечь сюда» 7-Zip: рядом с архивом; совпавшие имена не
        // затираются — новые получают «_1» (`-aou`).
        let args = format!("x -y -aou -bso1 -bse1 -bsp1 -sccUTF-8 {} {}", quoted(&format!("-o{dir}")), quoted(archive));
        let password = self.password.clone();
        self.run(JobKind::Extract { dest: dir, open: None, show: false, then_delete: Vec::new() }, archive, &args, password.as_deref());
    }

    /// «Распаковать в "имя\"» (25.11): a folder named as the archive next
    /// to it (7zz makes it), the archive goes inside.
    fn extract_to_folder(&mut self, archive: &str) {
        let dest = join(&parent(archive), &sysconf::filetypes::archive_stem(file_name(archive)));
        let _ = mkdir(&dest, 0o755);
        let args = format!("x -y -aou -bso1 -bse1 -bsp1 -sccUTF-8 {} {}", quoted(&format!("-o{dest}")), quoted(archive));
        let password = self.password.clone();
        self.run(JobKind::Extract { dest, open: None, show: false, then_delete: Vec::new() }, archive, &args, password.as_deref());
    }

    /// Test `archive` (the open one or one of `--test`).
    fn test_archive(&mut self, archive: &str) {
        let args = format!("t -bso1 -bse1 -bsp1 -sccUTF-8 {}", quoted(archive));
        let password = self.password.clone();
        self.run(JobKind::Test, archive, &args, password.as_deref());
    }

    /// «CRC SHA ▸ method» (25.11): checksums of `paths` on disk.
    fn hash(&mut self, paths: &[String], method: &str) {
        let Some(list) = list_file(paths) else {
            self.overlay = Overlay::Message(String::from("Не удалось посчитать"), String::from("Не записать список файлов в /tmp."), true);
            return;
        };
        let what = match paths {
            [one] => file_name(one).to_string(),
            many => format!("{} объект(ов)", many.len()),
        };
        let args = format!("h -bso1 -bse1 -sccUTF-8 -scrc{method} -scsUTF-8 {}", quoted(&format!("@{list}")));
        let first = paths.first().cloned().unwrap_or_default();
        self.run(JobKind::Hash { method: method.to_string(), what }, &first, &args, None);
    }

    /// The next archive of the action window.
    fn next_in_queue(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        let next = self.queue.remove(0);
        match self.queue_kind {
            Queue::Here => self.extract_here(&next),
            Queue::To => self.extract_to_folder(&next),
            Queue::Test => self.test_archive(&next),
        }
        true
    }

    fn test(&mut self) {
        if self.archive.is_empty() {
            return;
        }
        let archive = self.archive.clone();
        self.test_archive(&archive);
    }

    fn add_files(&mut self, archive: &str, sources: &[String], format: usize, level: usize, password: Option<&str>) {
        let (_, key, _, levels, passwords, _) = FORMATS[format.min(FORMATS.len() - 1)];
        let mut args = format!("a -y -bso1 -bse1 -bsp1 -sccUTF-8 -t{key}");
        if levels {
            args.push_str(&format!(" -mx{}", LEVELS[level.min(LEVELS.len() - 1)].1));
        }
        if passwords && password.is_some() && key == "7z" {
            // Имена файлов — тоже под паролем, как галочка 7-Zip.
            args.push_str(" -mhe=on");
        }
        if passwords && password.is_some() && key == "zip" {
            args.push_str(" -mem=AES256");
        }
        args.push_str(&format!(" {}", quoted(archive)));
        let Some(list) = list_file(sources) else {
            self.overlay = Overlay::Message(String::from("Не удалось создать архив"), String::from("Не записать список файлов в /tmp."), true);
            return;
        };
        args.push_str(&format!(" -scsUTF-8 {}", quoted(&format!("@{list}"))));
        self.run(JobKind::Add { archive: archive.to_string() }, archive, &args, if passwords { password } else { None });
    }

    fn delete_chosen(&mut self) {
        let names = self.chosen_names();
        self.delete_names(names);
    }

    /// Inner paths of the chosen rows.
    fn chosen_names(&self) -> Vec<String> {
        self.chosen().into_iter().filter_map(|index| self.rows.get(index)).map(|row| self.inner_path(row)).collect()
    }

    fn delete_names(&mut self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let Some(list) = list_file(&names) else { return };
        let args = format!("d -y -bso1 -bse1 -bsp1 -sccUTF-8 {} -scsUTF-8 {}", quoted(&self.archive), quoted(&format!("@{list}")));
        let password = self.password.clone();
        let archive = self.archive.clone();
        self.run(JobKind::Delete, &archive, &args, password.as_deref());
    }

    /// 7zz закончил.
    fn finished(&mut self, job: Job, code: i64) {
        let text = job.text();
        let seconds = (monotonic_ms().saturating_sub(job.started) + 500) / 1000;
        println(&format!("archiver: 7zz #{} exited with {code} after {seconds} s", job.task));
        let ok = code == 0 || (code == 1 && !text.contains("ERROR"));
        if !ok && wants_password(&text) {
            println(&format!("archiver: password asked for '{}'", job.archive));
            let ask = Ask {
                text: if self.password.is_some() { String::from("Неверный пароль. Введите пароль ещё раз:") } else { format!("Архив «{}» зашифрован. Введите пароль:", file_name(&job.archive)) },
                retry: job.kind.clone(),
                line: job.line.clone(),
                archive: job.archive.clone(),
            };
            self.overlay = Overlay::Password(Line::new(""), ask);
            return;
        }
        match job.kind.clone() {
            JobKind::List => {
                if !ok {
                    let why = error_lines(&text);
                    println(&format!("archiver: cannot open '{}': {why}", self.archive));
                    self.overlay = Overlay::Message(format!("Не удаётся открыть «{}»", file_name(&self.archive)), if why.is_empty() { String::from("Это не архив или он повреждён.") } else { why }, true);
                    self.archive.clear();
                    self.items.clear();
                    self.rows.clear();
                    return;
                }
                let (info, items) = parse_listing(&text);
                println(&format!("archiver: opened '{}': {} archive, {} item(s)", self.archive, if info.kind.is_empty() { "?" } else { &info.kind }, items.len()));
                self.info = info;
                self.items = items;
                let keep = self.dir.clone();
                self.set_dir(&keep);
            }
            JobKind::Extract { dest, open, show, then_delete } => {
                if !ok {
                    let why = error_lines(&text);
                    println(&format!("archiver: extract to '{dest}' failed: {why}"));
                    self.overlay = Overlay::Message(String::from("Ошибка при извлечении"), why, true);
                    self.queue.clear();
                    return;
                }
                println(&format!("archiver: extracted '{}' to '{dest}'", job.archive));
                if let Some(target) = open {
                    open_with_default(&target);
                } else {
                    self.note = Some(format!("Извлечено в {dest}"));
                }
                if show {
                    let id = spawn(&format!("/bin/files {dest}"));
                    println(&format!("archiver: showing '{dest}' in Files ({id})"));
                }
                if !then_delete.is_empty() {
                    // «Переместить»: out — now away from the archive.
                    println(&format!("archiver: moving {} item(s) out of '{}'", then_delete.len(), job.archive));
                    self.note = Some(format!("Перемещено в {dest}"));
                    self.delete_names(then_delete);
                    return;
                }
                self.next_in_queue();
            }
            JobKind::Test => {
                self.tested.push((job.archive.clone(), ok));
                if ok {
                    println(&format!("archiver: test of '{}' ok", job.archive));
                } else {
                    println(&format!("archiver: test of '{}' failed: {}", job.archive, error_lines(&text)));
                }
                if self.next_in_queue() {
                    return;
                }
                let tested = core::mem::take(&mut self.tested);
                let bad: Vec<&(String, bool)> = tested.iter().filter(|(_, ok)| !*ok).collect();
                self.overlay = match (tested.len(), bad.is_empty()) {
                    (1, true) => Overlay::Message(String::from("Ошибок не найдено"), format!("Архив «{}» проверен: все файлы целы.", file_name(&job.archive)), false),
                    (1, false) => Overlay::Message(String::from("Архив повреждён"), error_lines(&text), true),
                    (n, true) => Overlay::Message(String::from("Ошибок не найдено"), format!("Проверено архивов: {n}, все файлы целы."), false),
                    (n, false) => Overlay::Message(
                        String::from("Есть ошибки"),
                        format!("Проверено архивов: {n}, с ошибками: {} ({}).", bad.len(), bad.iter().map(|(path, _)| file_name(path)).collect::<Vec<_>>().join(", ")),
                        true,
                    ),
                };
            }
            JobKind::Hash { method, what } => {
                if !ok {
                    let why = error_lines(&text);
                    println(&format!("archiver: checksum {method} failed: {why}"));
                    self.overlay = Overlay::Message(String::from("Не удалось посчитать"), why, true);
                    return;
                }
                let rows = hash_rows(&text);
                println(&format!("archiver: checksum {method} of {what}: {}", rows.iter().map(|(key, value)| format!("{key} {value}")).collect::<Vec<_>>().join("; ")));
                self.overlay = Overlay::Facts { title: format!("Контрольные суммы — {what}"), rows, copy: true };
            }
            JobKind::Add { archive } => {
                if !ok {
                    let why = error_lines(&text);
                    println(&format!("archiver: creating '{archive}' failed: {why}"));
                    self.overlay = Overlay::Message(String::from("Не удалось создать архив"), why, true);
                    self.quit_after = false;
                    return;
                }
                println(&format!("archiver: created '{archive}'"));
                self.note = Some(format!("Готово: {archive}"));
                if !self.quit_after {
                    // Архив открыт — перечитать (добавили в него).
                    if self.archive == archive || self.archive.is_empty() {
                        self.archive = archive;
                        self.list();
                    }
                }
            }
            JobKind::Delete => {
                if !ok {
                    let why = error_lines(&text);
                    self.overlay = Overlay::Message(String::from("Не удалось удалить"), why, true);
                    return;
                }
                println(&format!("archiver: deleted from '{}'", self.archive));
                self.list();
            }
        }
    }

    // ---- Окно выбора файла ----------------------------------------------------

    fn pick(&mut self, pick: Pick) {
        if self.dialog.is_some() {
            return;
        }
        let here = if self.archive.is_empty() { home_dir() } else { parent(&self.archive) };
        let options = match pick {
            Pick::OpenArchive => Options::open().title("Открыть архив").dir(&here).filters(alloc::vec![Filter::new("Архивы", &sysconf::filetypes::ARCHIVES), Filter::all()]),
            Pick::ExtractTo => {
                let dir = match &self.mode {
                    Mode::Extract(form) => parent(form.dest.text.trim_end_matches('/')),
                    _ => here,
                };
                Options::folder().title("Куда извлечь").dir(&dir)
            }
            Pick::AddFiles => Options::open().title("Добавить в архив").dir(&home_dir()),
            Pick::CopyTo => Options::folder().title("Копировать в").dir(&here),
            Pick::MoveTo => Options::folder().title("Переместить в").dir(&here),
            Pick::SaveArchive => {
                let (dir, name) = match &self.mode {
                    Mode::Add(form) => (parent(&form.name.text), file_name(&form.name.text).to_string()),
                    _ => (here, String::from("Архив.7z")),
                };
                Options::save(&name).title("Имя архива").dir(&dir)
            }
        };
        match Dialog::start(&options, "archiver") {
            Ok(dialog) => self.dialog = Some((dialog, pick)),
            Err(text) => self.note = Some(text),
        }
    }

    fn picked_path(&mut self, pick: Pick, path: String) {
        println(&format!("archiver: picked '{path}'"));
        match pick {
            Pick::OpenArchive => self.open_archive(&path),
            Pick::ExtractTo => match &mut self.mode {
                Mode::Extract(form) => form.dest = Line::new(&path),
                _ => {
                    let names = self.chosen_names_for_extract();
                    self.extract_items(&path, names, None, false);
                }
            },
            Pick::AddFiles => {
                let archive = self.archive.clone();
                let format = FORMATS.iter().position(|format| format.2 == sysconf::filetypes::extension(&archive)).unwrap_or(0);
                if !FORMATS[format].5 {
                    self.overlay = Overlay::Message(String::from("Нельзя добавить"), String::from("В архив этого вида (gz, bz2, xz) нельзя добавлять файлы: он хранит один файл."), true);
                    return;
                }
                let password = self.password.clone();
                self.add_files(&archive, &[path], format, 2, password.as_deref());
            }
            Pick::SaveArchive => {
                if let Mode::Add(form) = &mut self.mode {
                    form.name = Line::new(&path);
                }
            }
            // «Копировать» — the chosen rows out to the folder; «Переместить»
            // — and then away from the archive (25.11).
            Pick::CopyTo | Pick::MoveTo => {
                let names = self.chosen_names();
                if names.is_empty() {
                    return;
                }
                println(&format!("archiver: {} {} item(s) to '{path}'", if pick == Pick::MoveTo { "moving" } else { "copying" }, names.len()));
                let then_delete = if pick == Pick::MoveTo { names.clone() } else { Vec::new() };
                self.extract_then(&path, names, None, false, then_delete);
            }
        }
    }

    /// Что извлекать кнопкой «Извлечь…»: отмеченное, иначе всё.
    fn chosen_names_for_extract(&self) -> Vec<String> {
        let picked: Vec<String> = (0..self.rows.len()).filter(|index| self.picked.get(*index).copied().unwrap_or(false)).filter_map(|index| self.rows.get(index)).map(|row| self.inner_path(row)).collect();
        if !picked.is_empty() {
            return picked;
        }
        if self.dir.is_empty() { Vec::new() } else { alloc::vec![self.dir.clone()] }
    }

    // ---- Формы ------------------------------------------------------------------

    fn submit_add(&mut self) {
        let Mode::Add(form) = &mut self.mode else { return };
        let name = form.name.text.trim().to_string();
        if name.is_empty() || !name.starts_with('/') {
            form.message = Some(String::from("Укажите полный путь архива."));
            return;
        }
        let (_, _, ext, _, _, multi) = FORMATS[form.format];
        if !multi && (form.sources.len() > 1 || form.sources.iter().any(|source| is_dir(source))) {
            form.message = Some(format!("{ext}: только один файл. Для папок — 7z, zip или tar."));
            return;
        }
        let name = if sysconf::filetypes::extension(&name) == ext { name } else { format!("{name}.{ext}") };
        let sources = form.sources.clone();
        let (format, level) = (form.format, form.level);
        let password = (!form.password.is_empty()).then(|| form.password.clone());
        self.quit_after = true;
        self.add_files(&name, &sources, format, level, password.as_deref());
    }

    fn submit_extract(&mut self) {
        let Mode::Extract(form) = &mut self.mode else { return };
        let dest = form.dest.text.trim().trim_end_matches('/').to_string();
        if dest.is_empty() || !dest.starts_with('/') {
            form.message = Some(String::from("Укажите полный путь папки."));
            return;
        }
        let (archive, show) = (form.archive.clone(), form.show);
        self.archive = archive;
        self.quit_after = true;
        self.extract_items(&dest, Vec::new(), None, show);
    }

    fn close_overlay(&mut self) {
        let failed = matches!(self.overlay, Overlay::Message(_, _, true));
        self.overlay = Overlay::None;
        // Окно-действие (`--extract-here`, `--zip`) после ошибки — закрыть.
        if self.quit_after && self.job.is_none() && !matches!(self.mode, Mode::Add(_) | Mode::Extract(_)) {
            self.done = true;
        }
        let _ = failed;
    }

    // ---- Menu bar and toolbar (25.11) -------------------------------------

    /// Items of menu `menu` of the bar.
    fn bar_items(&self, menu: usize) -> Vec<BarItem> {
        let open = !self.archive.is_empty();
        let rows = !self.rows.is_empty();
        let item = |label: &str, keys: Option<&'static str>, cmd: Cmd, group: u8, enabled: bool| BarItem { label: label.to_string(), keys, cmd, group, enabled };
        match menu {
            0 => alloc::vec![
                item("Открыть", Some("Enter"), Cmd::Open, 0, rows),
                item("Открыть архив…", Some("Ctrl+O"), Cmd::OpenArchive, 0, true),
                item("Создать архив…", None, Cmd::CreateArchive, 0, true),
                item("Добавить файлы…", None, Cmd::Add, 1, open),
                item("Извлечь…", Some("Ctrl+E"), Cmd::Extract, 1, open),
                item("Тестировать", Some("Ctrl+T"), Cmd::Test, 1, open),
                item("Копировать в…", None, Cmd::CopyTo, 2, rows),
                item("Переместить в…", None, Cmd::MoveTo, 2, rows),
                item("Удалить", Some("Del"), Cmd::Delete, 2, rows),
                item("Свойства архива", Some("Ctrl+I"), Cmd::Info, 3, open),
                item("Закрыть архив", None, Cmd::CloseArchive, 4, open),
                item("Выход", Some("Alt+F4"), Cmd::Exit, 4, true),
            ],
            1 => alloc::vec![
                item("Выделить всё", Some("Ctrl+A"), Cmd::SelectAll, 0, rows),
                item("Снять выделение", Some("Esc"), Cmd::SelectNone, 0, rows),
                item("Обратить выделение", None, Cmd::Invert, 0, rows),
            ],
            2 => {
                let mut items: Vec<BarItem> = COLUMNS
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| matches!(index, 0 | 1 | 2 | 3 | 5 | 7))
                    .map(|(index, (title, _, _))| {
                        let mark = if self.sort.0 == index { if self.sort.1 { "  ↓" } else { "  ↑" } } else { "" };
                        BarItem { label: format!("Упорядочить: {}{mark}", if *title == "CRC" { title.to_string() } else { title.to_lowercase() }), keys: None, cmd: Cmd::Sort(index as u8), group: 0, enabled: open }
                    })
                    .collect();
                items.push(item("Вверх на один уровень", Some("Backspace"), Cmd::Up, 1, open && !self.dir.is_empty()));
                items.push(item("Обновить", Some("Ctrl+R"), Cmd::Refresh, 1, open));
                items
            }
            3 => {
                let mut items = alloc::vec![item("Добавить папку в избранное", None, Cmd::FavAdd, 0, open)];
                for (index, (archive, dir)) in favorites().iter().enumerate() {
                    let label = if dir.is_empty() { file_name(archive).to_string() } else { format!("{}/{dir}", file_name(archive)) };
                    items.push(item(&label, None, Cmd::Fav(index as u8), 1, true));
                }
                if items.len() > 1 {
                    items.push(item("Очистить избранное", None, Cmd::FavClear, 2, true));
                }
                items
            }
            4 => {
                let mut items: Vec<BarItem> = HASHES
                    .iter()
                    .enumerate()
                    .map(|(index, (word, _))| {
                        let label = if *word == "*" { String::from("Контрольные суммы архива: все") } else { format!("Контрольная сумма архива: {word}") };
                        BarItem { label, keys: None, cmd: Cmd::Hash(index as u8), group: 0, enabled: open }
                    })
                    .collect();
                items.push(item("Тестировать архив", None, Cmd::Test, 1, open));
                items
            }
            _ => alloc::vec![item("О программе 7-Zip", None, Cmd::About, 0, true)],
        }
    }

    /// Run a command of the bar, the toolbar or a shortcut.
    fn run_cmd(&mut self, cmd: Cmd) {
        self.bar = None;
        println(&format!("archiver: command {cmd:?}"));
        let open = !self.archive.is_empty();
        match cmd {
            Cmd::Open => self.open_selected(),
            Cmd::OpenArchive => self.pick(Pick::OpenArchive),
            Cmd::CreateArchive => self.start_add_form(Vec::new()),
            Cmd::Add if open => self.pick(Pick::AddFiles),
            Cmd::Extract if open => self.pick(Pick::ExtractTo),
            Cmd::Test if open => self.test(),
            Cmd::CopyTo if !self.rows.is_empty() => self.pick(Pick::CopyTo),
            Cmd::MoveTo if !self.rows.is_empty() => self.pick(Pick::MoveTo),
            Cmd::Delete if !self.rows.is_empty() => self.overlay = Overlay::Delete(1),
            Cmd::Info if open => self.show_info(),
            Cmd::CloseArchive => {
                self.archive.clear();
                self.items.clear();
                self.rows.clear();
                self.dir.clear();
                self.info = Info::default();
                self.type_arg.clear();
            }
            Cmd::Exit => self.done = true,
            Cmd::SelectAll => self.picked = alloc::vec![true; self.rows.len()],
            Cmd::SelectNone => self.picked.iter_mut().for_each(|slot| *slot = false),
            Cmd::Invert => {
                self.picked.resize(self.rows.len(), false);
                self.picked.iter_mut().for_each(|slot| *slot = !*slot);
            }
            Cmd::Sort(column) => self.sort_by(usize::from(column)),
            Cmd::Up => {
                self.go_up();
            }
            Cmd::Refresh if open => self.list(),
            Cmd::FavAdd if open => {
                let mut list = favorites();
                list.retain(|(archive, dir)| !(archive == &self.archive && dir == &self.dir));
                list.insert(0, (self.archive.clone(), self.dir.clone()));
                list.truncate(FAVORITES);
                save_favorites(&list);
                println(&format!("archiver: favorite '{}' '/{}'", self.archive, self.dir));
                self.note = Some(String::from("Добавлено в избранное"));
            }
            Cmd::Fav(index) => {
                if let Some((archive, dir)) = favorites().get(usize::from(index)).cloned() {
                    self.type_arg.clear();
                    self.open_archive(&archive);
                    self.dir = dir;
                }
            }
            Cmd::FavClear => save_favorites(&[]),
            Cmd::Hash(index) if open => {
                let method = HASHES[usize::from(index).min(HASHES.len() - 1)].1;
                let archive = alloc::vec![self.archive.clone()];
                self.hash(&archive, method);
            }
            Cmd::About => {
                let banner = if self.info.banner.is_empty() { String::from("7-Zip 26.06") } else { self.info.banner.clone() };
                self.overlay = Overlay::Facts {
                    title: String::from("О программе 7-Zip"),
                    rows: alloc::vec![
                        (String::from("Версия"), banner.split(" : ").next().unwrap_or(&banner).to_string()),
                        (String::from("Автор"), String::from("Igor Pavlov, 7-zip.org (GNU LGPL)")),
                        (String::from("Программа"), String::from(SEVENZIP)),
                        (String::from("Окно"), String::from("Архиватор 7-Zip FossAi")),
                    ],
                    copy: false,
                };
            }
            _ => {}
        }
    }

    /// Sort by column `column`; the same column again — the other way.
    fn sort_by(&mut self, column: usize) {
        self.sort = if self.sort.0 == column { (column, !self.sort.1) } else { (column, false) };
        let name = self.rows.get(self.selected).map(|row| row.name.clone());
        sort_rows(&mut self.rows, self.sort.0, self.sort.1);
        self.picked = alloc::vec![false; self.rows.len()];
        if let Some(at) = name.and_then(|name| self.rows.iter().position(|row| row.name == name)) {
            self.selected = at;
        }
        println(&format!("archiver: sorted by {} {}", COLUMNS[column].0, if self.sort.1 { "desc" } else { "asc" }));
    }

    /// «Информация» (25.11): the chosen entry and the archive.
    fn show_info(&mut self) {
        let mut rows: Vec<(String, String)> = Vec::new();
        if let Some(row) = self.rows.get(self.selected) {
            rows.push((String::from("Имя"), self.inner_path(row)));
            for column in 1..COLUMNS.len() {
                let value = row.cell(column);
                if !value.is_empty() && !(column == 9 && !row.folder) {
                    rows.push((COLUMNS[column].0.to_string(), value));
                }
            }
            if row.folder {
                rows.push((String::from("Файлы"), row.files.to_string()));
            }
        }
        // A blank row: the archive goes into the second column.
        if !rows.is_empty() {
            rows.push((String::new(), String::new()));
        }
        let files = self.items.iter().filter(|item| !item.folder).count();
        let folders = self.items.iter().filter(|item| item.folder).count();
        let size: u64 = self.items.iter().map(|item| item.size).sum();
        let packed: u64 = self.items.iter().map(|item| item.packed).sum();
        rows.push((String::from("Архив"), self.archive.clone()));
        rows.push((String::from("Тип"), if self.info.kind.is_empty() { String::from("?") } else { self.info.kind.clone() }));
        if !self.info.method.is_empty() {
            rows.push((String::from("Метод"), self.info.method.clone()));
        }
        rows.push((String::from("Непрерывный"), String::from(if self.info.solid { "+" } else { "-" })));
        if !self.info.blocks.is_empty() {
            rows.push((String::from("Блоков"), self.info.blocks.clone()));
        }
        rows.push((String::from("Физический размер"), grouped(self.info.physical)));
        if self.info.headers > 0 {
            rows.push((String::from("Размер заголовков"), grouped(self.info.headers)));
        }
        rows.push((String::from("Папок / файлов"), format!("{folders} / {files}")));
        rows.push((String::from("Размер / сжатый"), format!("{} / {}", grouped(size), grouped(packed))));
        if size > 0 {
            let physical = if packed > 0 { packed } else { self.info.physical };
            rows.push((String::from("Степень сжатия"), format!("{} %", physical.saturating_mul(100) / size)));
        }
        println(&format!("archiver: info of '{}' ({} rows)", self.archive, rows.len()));
        self.overlay = Overlay::Facts { title: String::from("Информация"), rows, copy: false };
    }

    fn submit_password(&mut self) {
        let Overlay::Password(line, ask) = core::mem::replace(&mut self.overlay, Overlay::None) else { return };
        if line.text.is_empty() {
            self.overlay = Overlay::Password(line, ask);
            return;
        }
        self.password = Some(line.text.clone());
        let args = ask.line.strip_prefix(SEVENZIP).unwrap_or(&ask.line).trim().to_string();
        let password = self.password.clone();
        self.run(ask.retry, &ask.archive, &args, password.as_deref());
    }
}

fn save_favorites(list: &[(String, String)]) {
    for index in 0..FAVORITES {
        let key = format!("archiver/favorites/{index}");
        let _ = match list.get(index) {
            Some((archive, dir)) => user_progs::reg_set("user", &key, &format!("{archive}\t{dir}")),
            None => user_progs::reg_remove("user", &key),
        };
    }
}

/// Rows of the «CRC SHA» window from `7zz h` (25.11): «CRC32  for data:
/// 4E2D…», «SHA256 for data and names: …», «Size: 12».
fn hash_rows(text: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in text.lines().map(|line| line.trim_matches(['\r', '\x08', ' '])) {
        if let Some((key, value)) = line.split_once(" for data and names:") {
            rows.push((format!("{} (данные и имена)", key.trim()), value.trim().to_string()));
        } else if let Some((key, value)) = line.split_once(" for data:") {
            rows.push((format!("{} (данные)", key.trim()), value.trim().to_string()));
        } else if let Some(value) = line.strip_prefix("Size:") {
            rows.push((String::from("Размер"), grouped(value.trim().parse().unwrap_or(0))));
        } else if let Some(value) = line.strip_prefix("Files:") {
            rows.push((String::from("Файлов"), value.trim().to_string()));
        } else if let Some(value) = line.strip_prefix("Folders:") {
            rows.push((String::from("Папок"), value.trim().to_string()));
        }
    }
    rows
}

/// Открыть файл программой по умолчанию (первая из «Открыть с помощью»).
fn open_with_default(path: &str) {
    let list = user_progs::open_with_list(path);
    let command = list.lines().find_map(|line| {
        let mut parts = line.split('\t');
        let _mark = parts.next()?;
        let _caption = parts.next()?;
        parts.next().filter(|command| !command.is_empty()).map(String::from)
    });
    let Some(command) = command else {
        println(&format!("archiver: no program for '{path}'"));
        return;
    };
    let line = format!("{command} {path}");
    let id = spawn(&line);
    println(&format!("archiver: opened '{path}' with '{command}' ({id})"));
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

impl App for Archiver {
    fn size_limits(&self) -> app::SizeLimits {
        match self.mode {
            Mode::Browse => app::SizeLimits::min(640, 440),
            Mode::Add(_) => app::SizeLimits::fixed(560, 470),
            Mode::Extract(_) => app::SizeLimits::fixed(560, 300),
        }
    }

    fn resizable(&self) -> bool {
        matches!(self.mode, Mode::Browse)
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn title(&self) -> Option<String> {
        let what = match &self.mode {
            Mode::Add(_) => String::from("Добавить в архив"),
            Mode::Extract(form) => format!("Извлечь — {}", file_name(&form.archive)),
            Mode::Browse if self.archive.is_empty() => String::from("Архиватор 7-Zip"),
            Mode::Browse => format!("{} — 7-Zip", file_name(&self.archive)),
        };
        Some(format!("7-Zip\x1f{what}"))
    }

    fn close_requested(&mut self) -> bool {
        if let Some(job) = self.job.as_mut() {
            job.cancel();
        }
        true
    }

    fn step(&mut self) -> bool {
        let mut redraw = false;
        if let Some((dialog, pick)) = self.dialog.as_mut() {
            let pick = *pick;
            if let Some(outcome) = dialog.poll() {
                self.dialog = None;
                if let Outcome::Chosen(path) = outcome {
                    self.picked_path(pick, path);
                }
                redraw = true;
            }
        }
        if let Some(job) = self.job.as_mut() {
            let before = (job.percent, job.current.len());
            match job.poll() {
                Some(code) => {
                    let job = self.job.take().unwrap();
                    self.finished(job, code);
                    if self.quit_after && self.job.is_none() && matches!(self.overlay, Overlay::None) {
                        // Каркас спрашивает `wants_quit` только после событий
                        // окна, а их у окна-действия может и не быть: выйти
                        // самим (стол закроет окно вместе с задачей).
                        println("archiver: closing on request after the job");
                        user_progs::exit(0);
                    }
                    redraw = true;
                }
                None => redraw |= before != (job.percent, job.current.len()),
            }
        }
        redraw
    }

    fn tick(&mut self, _info: &user_progs::SysInfo) -> bool {
        // Секунды на карточке хода.
        self.job.is_some()
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & WIN_MOD_CTRL != 0;
        let shift = mods & WIN_MOD_SHIFT != 0;
        if self.dialog.is_some() {
            return false;
        }
        if let Some(job) = self.job.as_mut() {
            if code == ESCAPE {
                job.cancel();
            }
            return true;
        }
        // Вопросы — модальные.
        match &mut self.overlay {
            Overlay::Password(line, _) => {
                match code {
                    ENTER => self.submit_password(),
                    ESCAPE => {
                        println("archiver: password cancelled");
                        self.overlay = Overlay::None;
                        if self.quit_after {
                            self.done = true;
                        }
                    }
                    _ => {
                        line.key(code, mods, latin);
                    }
                }
                return true;
            }
            Overlay::Message(..) | Overlay::Facts { .. } => {
                if matches!(code, ENTER | ESCAPE | SPACE) {
                    self.close_overlay();
                }
                return true;
            }
            Overlay::Delete(focus) => {
                match code {
                    TAB | user_progs::WIN_KEY_LEFT | user_progs::WIN_KEY_RIGHT => *focus = 1 - (*focus).min(1),
                    ENTER if *focus == 0 => {
                        self.overlay = Overlay::None;
                        self.delete_chosen();
                    }
                    ENTER | ESCAPE => self.overlay = Overlay::None,
                    _ => {}
                }
                return true;
            }
            Overlay::None => {}
        }
        match &mut self.mode {
            Mode::Add(form) => {
                form.message = None;
                match code {
                    ENTER => self.submit_add(),
                    ESCAPE => self.done = true,
                    TAB => {
                        form.focus = match (form.focus, shift) {
                            (AddFocus::Name, false) | (AddFocus::Level, true) => AddFocus::Format,
                            (AddFocus::Format, false) | (AddFocus::Password, true) => AddFocus::Level,
                            (AddFocus::Level, false) | (AddFocus::Name, true) => AddFocus::Password,
                            (AddFocus::Password, false) | (AddFocus::Format, true) => AddFocus::Name,
                        };
                    }
                    user_progs::WIN_KEY_LEFT | user_progs::WIN_KEY_RIGHT if matches!(form.focus, AddFocus::Format | AddFocus::Level) => {
                        let right = code == user_progs::WIN_KEY_RIGHT;
                        let step = |value: usize, len: usize| if right { (value + 1).min(len - 1) } else { value.saturating_sub(1) };
                        if form.focus == AddFocus::Format {
                            form.format = step(form.format, FORMATS.len());
                            retarget(form);
                        } else {
                            form.level = step(form.level, LEVELS.len());
                        }
                    }
                    _ => match form.focus {
                        AddFocus::Name => {
                            form.name.key(code, mods, latin);
                        }
                        AddFocus::Password => match code {
                            BACKSPACE => {
                                form.password.pop();
                            }
                            code if code < 0x0100_0000 && !ctrl => {
                                if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                                    form.password.push(ch);
                                }
                            }
                            _ => return false,
                        },
                        _ => return false,
                    },
                }
                return true;
            }
            Mode::Extract(form) => {
                form.message = None;
                match code {
                    ENTER => self.submit_extract(),
                    ESCAPE => self.done = true,
                    TAB => form.focus = if form.focus == ExtractFocus::Dest { ExtractFocus::Show } else { ExtractFocus::Dest },
                    SPACE if form.focus == ExtractFocus::Show => form.show = !form.show,
                    _ if form.focus == ExtractFocus::Dest => {
                        form.dest.key(code, mods, latin);
                    }
                    _ => return false,
                }
                return true;
            }
            Mode::Browse => {}
        }
        // The open menu of the bar takes the keys (25.11).
        if let Some((menu, selected)) = self.bar {
            let items = self.bar_items(menu);
            let step = |from: usize, down: bool| -> usize {
                let n = items.len();
                if n == 0 {
                    return from;
                }
                let mut at = if from >= n { if down { n - 1 } else { 0 } } else { from };
                for _ in 0..n {
                    at = if down { (at + 1) % n } else { (at + n - 1) % n };
                    if items[at].enabled {
                        return at;
                    }
                }
                from
            };
            match code {
                ESCAPE => self.bar = None,
                user_progs::WIN_KEY_LEFT => self.bar = Some(((menu + MENUS.len() - 1) % MENUS.len(), usize::MAX)),
                user_progs::WIN_KEY_RIGHT => self.bar = Some(((menu + 1) % MENUS.len(), usize::MAX)),
                WIN_KEY_UP => self.bar = Some((menu, step(selected, false))),
                WIN_KEY_DOWN => self.bar = Some((menu, step(selected, true))),
                ENTER => {
                    if let Some(item) = items.get(selected).filter(|item| item.enabled) {
                        let cmd = item.cmd;
                        self.run_cmd(cmd);
                    }
                }
                _ => {}
            }
            return true;
        }
        if ctrl {
            match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                Some('o') => self.run_cmd(Cmd::OpenArchive),
                Some('e') => self.run_cmd(Cmd::Extract),
                Some('t') => self.run_cmd(Cmd::Test),
                Some('a') => self.run_cmd(Cmd::SelectAll),
                Some('r') => self.run_cmd(Cmd::Refresh),
                Some('i') => self.run_cmd(Cmd::Info),
                _ => return false,
            }
            return true;
        }
        let count = self.rows.len();
        match code {
            WIN_KEY_UP => self.selected = self.selected.saturating_sub(1),
            WIN_KEY_DOWN => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            WIN_KEY_HOME => self.selected = 0,
            WIN_KEY_END => self.selected = count.saturating_sub(1),
            WIN_KEY_PAGE_UP => self.selected = self.selected.saturating_sub(10),
            WIN_KEY_PAGE_DOWN => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            ENTER => self.open_selected(),
            BACKSPACE => {
                self.go_up();
            }
            SPACE => {
                if let Some(slot) = self.picked.get_mut(self.selected) {
                    *slot = !*slot;
                }
            }
            WIN_KEY_DELETE if !self.rows.is_empty() => self.overlay = Overlay::Delete(1),
            ESCAPE => {
                self.picked.iter_mut().for_each(|slot| *slot = false);
            }
            _ => return false,
        }
        if self.selected < count {
            self.scroll.follow(self.selected);
            println(&format!("archiver: selected '{}'", self.rows[self.selected].name));
        }
        true
    }

    fn click_with(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32, mods: i32) -> bool {
        if self.dialog.is_some() {
            return false;
        }
        if self.job.is_some() {
            let card = progress_card(ctx, area);
            if card_buttons(ctx, card)[1].contains(x, y) {
                if let Some(job) = self.job.as_mut() {
                    job.cancel();
                }
            }
            return true;
        }
        match &mut self.overlay {
            Overlay::Password(line, _) => {
                let card = ask_card(ctx, area);
                let [ok, cancel] = card_buttons(ctx, card);
                let field = password_field(ctx, card);
                if ok.contains(x, y) {
                    self.submit_password();
                } else if cancel.contains(x, y) {
                    self.overlay = Overlay::None;
                    if self.quit_after {
                        self.done = true;
                    }
                } else if field.contains(x, y) {
                    let _ = line;
                }
                return true;
            }
            Overlay::Message(..) => {
                let card = ask_card(ctx, area);
                if card_buttons(ctx, card)[1].contains(x, y) {
                    self.close_overlay();
                }
                return true;
            }
            Overlay::Facts { rows, copy, .. } => {
                let groups = fact_groups(rows);
                let card = facts_card(ctx, area, groups.iter().map(|group| group.len()).max().unwrap_or(0), groups.len());
                let [copy_button, ok] = card_buttons(ctx, card);
                if ok.contains(x, y) {
                    self.close_overlay();
                } else if *copy && copy_button.contains(x, y) {
                    let text: Vec<String> = rows.iter().filter(|(key, _)| !key.is_empty()).map(|(key, value)| format!("{key}: {value}")).collect();
                    if user_progs::clipboard_set(&text.join("\n")) {
                        println("archiver: checksums copied");
                    }
                }
                return true;
            }
            Overlay::Delete(_) => {
                let ask = delete_ask(self.chosen().len());
                match kit::ask_hit(ctx, area, &ask, x, y) {
                    Some(true) => {
                        self.overlay = Overlay::None;
                        self.delete_chosen();
                    }
                    Some(false) => self.overlay = Overlay::None,
                    None => {}
                }
                return true;
            }
            Overlay::None => {}
        }
        match &mut self.mode {
            Mode::Add(form) => {
                let plan = FormPlan::new(ctx, area, 4);
                form.message = None;
                if plan.fields[0].contains(x, y) {
                    form.focus = AddFocus::Name;
                    let face = ctx.face(Role::Body);
                    form.name.click(face, plan.fields[0].w.saturating_sub(ctx.px(20)), x - plan.fields[0].x - ctx.px(10) as i32);
                } else if plan.browse.contains(x, y) {
                    self.pick(Pick::SaveArchive);
                } else if plan.fields[1].contains(x, y) {
                    form.focus = AddFocus::Format;
                    if let Some(index) = chip_at(ctx, plan.fields[1], FORMATS.iter().map(|format| format.0), x, y) {
                        form.format = index;
                        retarget(form);
                    }
                } else if plan.fields[2].contains(x, y) {
                    form.focus = AddFocus::Level;
                    if let Some(index) = chip_at(ctx, plan.fields[2], LEVELS.iter().map(|level| level.0), x, y) {
                        form.level = index;
                    }
                } else if plan.fields[3].contains(x, y) {
                    form.focus = AddFocus::Password;
                } else if plan.ok.contains(x, y) {
                    self.submit_add();
                } else if plan.cancel.contains(x, y) {
                    self.done = true;
                }
                return true;
            }
            Mode::Extract(form) => {
                let plan = FormPlan::new(ctx, area, 1);
                form.message = None;
                let show = show_box(ctx, &plan);
                if plan.fields[0].contains(x, y) {
                    form.focus = ExtractFocus::Dest;
                    let face = ctx.face(Role::Body);
                    form.dest.click(face, plan.fields[0].w.saturating_sub(ctx.px(20)), x - plan.fields[0].x - ctx.px(10) as i32);
                } else if plan.browse.contains(x, y) {
                    self.pick(Pick::ExtractTo);
                } else if show.contains(x, y) {
                    form.focus = ExtractFocus::Show;
                    form.show = !form.show;
                } else if plan.ok.contains(x, y) {
                    self.submit_extract();
                } else if plan.cancel.contains(x, y) {
                    self.done = true;
                }
                return true;
            }
            Mode::Browse => {}
        }
        let plan = Plan::new(ctx, area);
        // The menu bar (25.11): an open menu takes the click first.
        if let Some((menu, _)) = self.bar {
            let items = self.bar_items(menu);
            let rect = dropdown_rect(ctx, area, plan.heads[menu], &items);
            if rect.contains(x, y) {
                let entries = bar_entries(&items);
                if let Some(index) = kit::menu_hit(ctx, rect, &entries, x, y) {
                    if items[index].enabled {
                        let cmd = items[index].cmd;
                        self.run_cmd(cmd);
                    }
                }
                return true;
            }
            self.bar = match plan.heads.iter().position(|head| head.contains(x, y)) {
                Some(head) if head != menu => Some((head, usize::MAX)),
                _ => None,
            };
            return true;
        }
        if let Some(head) = plan.heads.iter().position(|head| head.contains(x, y)) {
            self.bar = Some((head, usize::MAX));
            println(&format!("archiver: menu '{}' opened", MENUS[head]));
            return true;
        }
        if let Some(index) = plan.buttons.iter().position(|rect| rect.contains(x, y)) {
            println(&format!("archiver: toolbar '{}'", TOOLS[index]));
            let cmd = if TOOL_CMDS[index] == Cmd::Add && self.archive.is_empty() { Cmd::CreateArchive } else { TOOL_CMDS[index] };
            self.run_cmd(cmd);
            return true;
        }
        if plan.up.contains(x, y) {
            return self.go_up();
        }
        if self.archive.is_empty() {
            let [open, create] = empty_buttons(ctx, plan.table.list);
            if open.contains(x, y) {
                self.pick(Pick::OpenArchive);
            } else if create.contains(x, y) {
                self.start_add_form(Vec::new());
            }
            return true;
        }
        if plan.table.header.contains(x, y) {
            if let Some(column) = plan.column_at(x) {
                self.sort_by(column);
            }
            return true;
        }
        let list = plan.table.list;
        let content = self.rows.len() as u32 * plan.step();
        if let Some(bar) = ScrollBar::vertical(ctx, list, content, &self.scroll) {
            if bar.press(&self.scroll, x, y) {
                return true;
            }
        }
        if !list.contains(x, y) {
            return false;
        }
        let Some(index) = self.scroll.row_at(y, list.y, plan.step()).filter(|index| *index < self.rows.len()) else {
            return false;
        };
        let now = monotonic_ms();
        if mods & WIN_MOD_CTRL != 0 {
            if let Some(slot) = self.picked.get_mut(index) {
                *slot = !*slot;
            }
            self.selected = index;
            return true;
        }
        let double = self.last_click.1 == index && now.saturating_sub(self.last_click.0) <= DOUBLE_CLICK_MS;
        self.last_click = (now, index);
        self.selected = index;
        self.picked.iter_mut().for_each(|slot| *slot = false);
        if double {
            self.last_click = (0, usize::MAX);
            self.open_selected();
        }
        true
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if !matches!(self.mode, Mode::Browse) || self.archive.is_empty() {
            return false;
        }
        let plan = Plan::new(ctx, area);
        let content = self.rows.len() as u32 * plan.step();
        ScrollBar::vertical(ctx, plan.table.list, content, &self.scroll).is_some_and(|bar| bar.drag(&self.scroll, x, y))
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.scroll.release()
    }

    /// Pointer over the window (25.11): the lit item of the open menu
    /// follows it, the menus of the bar switch under it, a toolbar button
    /// under it is lit.
    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if !matches!(self.mode, Mode::Browse) || self.quit_after {
            return false;
        }
        let plan = Plan::new(ctx, area);
        let before = (self.bar, self.hot);
        if let Some((menu, _)) = self.bar {
            if let Some(head) = plan.heads.iter().position(|head| head.contains(x, y)) {
                if head != menu {
                    self.bar = Some((head, usize::MAX));
                }
            } else {
                let items = self.bar_items(menu);
                let rect = dropdown_rect(ctx, area, plan.heads[menu], &items);
                if let Some(index) = kit::menu_hit(ctx, rect, &bar_entries(&items), x, y) {
                    self.bar = Some((menu, index));
                }
            }
        }
        self.hot = if self.bar.is_none() { plan.buttons.iter().position(|rect| rect.contains(x, y)) } else { None };
        before != (self.bar, self.hot)
    }

    fn leave(&mut self) -> bool {
        self.hot.take().is_some()
    }

    fn wheel_at(&mut self, area: Rect, ctx: Ctx, clicks: i32) -> bool {
        if !matches!(self.mode, Mode::Browse) {
            return false;
        }
        let plan = Plan::new(ctx, area);
        self.scroll.wheel(clicks, plan.table.list.h, self.rows.len() as u32 * plan.step(), plan.step())
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        match &self.mode {
            Mode::Add(form) => draw_add(ctx, s, area, form),
            Mode::Extract(form) => draw_extract(ctx, s, area, form),
            // Окно-действие («Извлечь сюда», «Сжать в ZIP») — только ход.
            Mode::Browse if self.quit_after => {}
            Mode::Browse => {
                self.draw_browser(ctx, s, area);
                if let Some((menu, selected)) = self.bar {
                    let plan = Plan::new(ctx, area);
                    let items = self.bar_items(menu);
                    let rect = dropdown_rect(ctx, area, plan.heads[menu], &items);
                    kit::draw_menu(ctx, s, rect, &bar_entries(&items), selected);
                }
            }
        }
        if let Some(job) = &self.job {
            if !matches!(job.kind, JobKind::List) || !self.archive.is_empty() {
                draw_progress(ctx, s, area, job);
            }
        }
        match &self.overlay {
            Overlay::None => {}
            Overlay::Password(line, ask) => draw_password(ctx, s, area, line, ask),
            Overlay::Message(title, text, bad) => draw_message(ctx, s, area, title, text, *bad),
            Overlay::Delete(focus) => kit::draw_ask(ctx, s, area, &delete_ask(self.chosen().len()), *focus),
            Overlay::Facts { title, rows, copy } => draw_facts(ctx, s, area, title, rows, *copy),
        }
    }
}

/// Entries of a menu of the bar for `kit`.
fn bar_entries(items: &[BarItem]) -> Vec<kit::Entry<'_>> {
    items
        .iter()
        .map(|item| kit::Entry { title: &item.label, keys: item.keys, group: item.group, disabled: !item.enabled, ..kit::Entry::NONE })
        .collect()
}

/// Where the menu of the bar under `head` drops.
fn dropdown_rect(ctx: Ctx, area: Rect, head: Rect, items: &[BarItem]) -> Rect {
    kit::place((head.x, head.bottom() + ctx.pxi(2)), kit::menu_size(ctx, &bar_entries(items)), area)
}

/// Groups of facts: a blank row starts the next column.
fn fact_groups(rows: &[(String, String)]) -> Vec<&[(String, String)]> {
    rows.split(|(key, _)| key.is_empty()).filter(|group| !group.is_empty()).collect()
}

/// The card of [`Overlay::Facts`]: `rows` lines (the longest column) and
/// buttons; two columns are wider.
fn facts_card(ctx: Ctx, area: Rect, rows: usize, columns: usize) -> Rect {
    let line = u32::from(ctx.face(Role::Body).line) + ctx.px(6);
    let h = ctx.px(20 + 34 + 20 + 32 + 20) + line * rows.max(1) as u32;
    let w = ctx.px(if columns > 1 { 820 } else { 640 }).min(area.w.saturating_sub(ctx.px(24)));
    let h = h.min(area.h.saturating_sub(ctx.px(16)));
    Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h)
}

fn draw_facts(ctx: Ctx, s: &mut Surface, area: Rect, title: &str, rows: &[(String, String)], copy: bool) {
    let p = ctx.palette;
    draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 70);
    let groups = fact_groups(rows);
    let longest = groups.iter().map(|group| group.len()).max().unwrap_or(0);
    let card = facts_card(ctx, area, longest, groups.len());
    let inner = kit::card(ctx, s, card);
    let pad = ctx.px(20) as i32;
    let g = ctx.px(20);
    iconset::draw(s, Fi::Info, card.x + pad, card.y + pad, g, p.acc, 255);
    paint::text_clipped(inner, s, Role::Title, card.x + pad + g as i32 + ctx.pxi(10), card.y + pad, card.w.saturating_sub(pad as u32 * 2 + g + ctx.px(10)), title, p.ink);
    let face = ctx.face(Role::Body);
    let line = i32::from(face.line) + ctx.pxi(6);
    let [copy_button, ok] = card_buttons(ctx, card);
    let column_w = (card.w.saturating_sub(pad as u32 * 2) / groups.len().max(1) as u32) as i32;
    for (at, group) in groups.iter().enumerate() {
        let left = card.x + pad + column_w * at as i32;
        let right = left + column_w - if at + 1 < groups.len() { ctx.pxi(16) } else { 0 };
        let key_w = group.iter().map(|(key, _)| face.width(key)).max().unwrap_or(0).min(column_w as u32 / 2) + ctx.px(16);
        let mut y = card.y + pad + ctx.pxi(34);
        for (key, value) in group.iter() {
            if y + line > copy_button.y - ctx.pxi(8) {
                break;
            }
            paint::text_clipped(inner, s, Role::Body, left, y, key_w, key, p.ink3);
            let vx = left + key_w as i32;
            paint::text_clipped(inner, s, Role::Body, vx, y, (right - vx).max(0) as u32, value, p.ink);
            y += line;
        }
    }
    if copy {
        paint::button(inner, s, copy_button, Weight::Normal, "Копировать", false);
    }
    paint::button(inner, s, ok, Weight::Primary, "ОК", false);
}

fn delete_ask(count: usize) -> kit::Ask<'static> {
    kit::Ask {
        icon: Icon::Close,
        tone: Tone::Bad,
        title: "Удалить из архива?",
        detail: if count == 1 { "Выбранный объект будет удалён из архива." } else { "Выбранные объекты будут удалены из архива." },
        text: "Файлы на диске не затрагиваются.",
        action: "Удалить",
        weight: Weight::Danger,
    }
}

/// Сменили формат — сменить и расширение имени архива.
fn retarget(form: &mut AddForm) {
    let name = form.name.text.clone();
    let ext = FORMATS[form.format].2;
    let base = stem(&name);
    form.name = Line::new(&format!("{base}.{ext}"));
}

impl Archiver {
    fn start_add_form(&mut self, sources: Vec<String>) {
        let first = sources.first().cloned().unwrap_or_else(|| join(&home_dir(), "Архив"));
        let dir = parent(&first);
        let base = if sources.len() == 1 { stem(file_name(&first)) } else { file_name(&dir).to_string() };
        let base = if base.is_empty() || base == "/" { String::from("Архив") } else { base };
        let name = free_name(&dir, &base, ".7z");
        self.mode = Mode::Add(AddForm { sources, name: Line::new(&name), format: 0, level: 2, password: String::new(), focus: AddFocus::Name, message: None });
        println(&format!("archiver: add form for '{name}'"));
    }

    fn draw_browser(&self, ctx: Ctx, s: &mut Surface, area: Rect) {
        let p = ctx.palette;
        let plan = Plan::new(ctx, area);
        let open = !self.archive.is_empty();

        // Menu bar (25.11, 7-Zip File Manager): Файл Правка Вид Избранное
        // Сервис Справка.
        let bar = kit::toolbar(ctx, s, plan.menubar);
        for (index, (label, head)) in MENUS.iter().zip(plan.heads).enumerate() {
            if self.bar.is_some_and(|(menu, _)| menu == index) {
                kit::menu_highlight(bar, s, head, false);
            }
            paint::text(bar, s, Role::Body, head.x + ctx.pxi(10), paint::baseline(bar, Role::Body, head), label, p.ink);
        }

        // Toolbar: big icons over words, in 7-Zip's order and colours.
        let tools = kit::toolbar(ctx, s, plan.toolbar);
        let tints = [
            mini_ui::Color::rgb(0x2E, 0xA0, 0x43),
            mini_ui::Color::rgb(0x2F, 0x6F, 0xDB),
            mini_ui::Color::rgb(0x16, 0xA2, 0xB8),
            mini_ui::Color::rgb(0x12, 0x8C, 0x78),
            mini_ui::Color::rgb(0x3D, 0x55, 0xC8),
            mini_ui::Color::rgb(0xD9, 0x3B, 0x3B),
            mini_ui::Color::rgb(0xD8, 0x9A, 0x12),
        ];
        let icons = [Fi::Plus, Fi::Download, Fi::Check, Fi::Copy, Fi::ArrowRight, Fi::X, Fi::Info];
        for (index, rect) in plan.buttons.iter().enumerate() {
            let enabled = match TOOL_CMDS[index] {
                Cmd::Add => true,
                Cmd::CopyTo | Cmd::MoveTo | Cmd::Delete => !self.rows.is_empty(),
                _ => open,
            };
            if self.hot == Some(index) && enabled {
                kit::menu_highlight(tools, s, *rect, false);
            }
            let g = ctx.px(26);
            let ink = if enabled { tints[index] } else { p.ink6 };
            iconset::draw(s, icons[index], rect.x + (rect.w as i32 - g as i32) / 2, rect.y + ctx.pxi(4), g, ink, 255);
            let label = Rect::new(rect.x, rect.y + g as i32 + ctx.pxi(6), rect.w, u32::from(ctx.face(Role::Caption).line));
            paint::text_centered(tools, s, Role::Caption, label, TOOLS[index], if enabled { p.ink2 } else { p.ink6 });
        }

        // Address row: «вверх» and the path, the archive as a folder of it.
        paint::icon_button(ctx, s, plan.up, Icon::Up, Weight::Ghost, false);
        paint::sunk(ctx, s, plan.address, ctx.px(theme::R_ROW));
        let g = ctx.px(18);
        let icon = Rect::new(plan.address.x + ctx.pxi(8), plan.address.y + (plan.address.h as i32 - g as i32) / 2, g, g);
        if !user_progs::icons::draw(s, icon, if open { "doc-archive" } else { "archiver" }) {
            iconset::draw(s, Fi::Compress, icon.x, icon.y, g, p.acc, 255);
        }
        let path = if open {
            let mut path = format!("{}/", self.archive);
            if !self.dir.is_empty() {
                path.push_str(&self.dir);
                path.push('/');
            }
            path
        } else {
            String::new()
        };
        let tx = icon.right() + ctx.pxi(8);
        paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, plan.address), (plan.address.right() - tx - ctx.pxi(8)).max(0) as u32, &path, p.ink);

        let table = plan.table;
        let list = table.list;
        if !open {
            if self.job.is_none() {
                draw_empty(ctx, s, list);
            }
            kit::status_bar(ctx, s, plan.status, "Откройте архив или создайте новый");
            return;
        }

        // Header: titles, the sorted column with its arrow.
        let columns = &plan.columns;
        // The line (or the classic buttons) of the shared header; the titles
        // here: numbers' titles on the right like the numbers, the sorted
        // one with its arrow.
        table.draw_header(ctx, s, columns, &[""; COLUMNS.len()]);
        let caps_face = ctx.face(Role::MonoCaps);
        let caps_width = |text: &str| caps_face.width(text) + theme::CAPS_TRACKING * ctx.scale * text.chars().count().saturating_sub(1) as u32;
        let head_y = paint::baseline(ctx, Role::MonoCaps, table.header);
        let a = ctx.px(10);
        for (index, rect) in columns.iter().enumerate() {
            if rect.w == 0 {
                continue;
            }
            let (title, _, numeric) = COLUMNS[index];
            let sorted = index == self.sort.0;
            let w = caps_width(title);
            let arrow = if sorted { a + ctx.px(4) } else { 0 };
            let x = if numeric { rect.right() - ctx.pxi(14) - (w + arrow) as i32 } else { rect.x };
            paint::caps(ctx, s, x, head_y, title);
            if sorted {
                glyphicon::draw(s, if self.sort.1 { Icon::ChevronDown } else { Icon::ChevronUp }, x + (w + ctx.px(4)) as i32, table.header.y + (table.header.h as i32 - a as i32) / 2, a, p.acc, 255);
            }
        }

        let step = plan.step();
        let content = self.rows.len() as u32 * step;
        self.scroll.sync(list.h, content, step);
        let face_line = ctx.px(20);
        for index in self.scroll.rows(list.h, step, self.rows.len()) {
            let row = &self.rows[index];
            let y = list.y + (index as u32 * step) as i32 - self.scroll.offset();
            let rect = Rect::new(list.x + ctx.px(12) as i32, y, list.w.saturating_sub(ctx.px(24)), step.saturating_sub(ctx.px(2)));
            if rect.bottom() <= list.y || rect.y >= list.bottom() {
                continue;
            }
            let picked = self.picked.get(index).copied().unwrap_or(false);
            let state = if index == self.selected || picked { RowState::Selected } else { RowState::Idle };
            paint::row(ctx, s, rect, state);
            let ink = paint::row_ink(ctx, state);
            let icon = Rect::new(columns[0].x, rect.y + (rect.h as i32 - face_line as i32) / 2, face_line, face_line);
            let kind = sysconf::filetypes::kind_of(&row.name, row.folder);
            if !user_progs::icons::draw(s, icon, kind.icon) {
                glyphicon::draw(s, if row.folder { Icon::Folder } else { Icon::File }, icon.x, icon.y, icon.w, p.ink3, 255);
            }
            kit::cell(ctx, s, columns[0], icon.right() + ctx.px(10) as i32, rect, Role::Body, &row.name, if state == RowState::Selected { p.ink } else { ink });
            for (column, rect_c) in columns.iter().enumerate().skip(1) {
                if rect_c.w == 0 {
                    continue;
                }
                let text = row.cell(column);
                if text.is_empty() {
                    continue;
                }
                let role = if matches!(column, 3 | 4 | 7 | 8) { Role::Caption } else { Role::Body };
                if COLUMNS[column].2 {
                    // Numbers on the right edge of their column.
                    let right = rect_c.right() - ctx.pxi(14);
                    paint::text_right(ctx, s, role, right, paint::baseline(ctx, role, rect), &text, ink);
                } else {
                    kit::cell(ctx, s, *rect_c, rect_c.x, rect, role, &text, ink);
                }
            }
        }
        if let Some(bar) = ScrollBar::vertical(ctx, list, content, &self.scroll) {
            bar.draw(ctx, s, &self.scroll);
        }
        if self.rows.is_empty() && self.job.is_none() {
            paint::text_centered(ctx, s, Role::Body, Rect::new(list.x, list.y, list.w, ctx.px(60)), "Папка пуста", p.ink4);
        }

        let picked = self.picked.iter().filter(|slot| **slot).count();
        let mut status = if picked > 0 {
            let bytes: u64 = self.rows.iter().zip(&self.picked).filter(|(_, on)| **on).map(|(row, _)| row.size).sum();
            kit::selected_text(picked, bytes)
        } else {
            kit::items_text(self.rows.len())
        };
        if !self.info.kind.is_empty() {
            status.push_str(&format!("   ·   {}", self.info.kind));
            if !self.info.method.is_empty() {
                status.push_str(&format!(" ({})", self.info.method));
            }
            if self.info.solid {
                status.push_str(", непрерывный");
            }
            if self.info.physical > 0 {
                status.push_str(&format!(", {}", size_text(self.info.physical)));
            }
        }
        if let Some(note) = &self.note {
            status = format!("{note}   ·   {status}");
        }
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

fn empty_buttons(ctx: Ctx, area: Rect) -> [Rect; 2] {
    let bw = ctx.px(170);
    let bh = ctx.px(34);
    let y = area.y + area.h as i32 / 2 + ctx.pxi(24);
    let gap = ctx.px(12) as i32;
    let x = area.x + (area.w as i32 - (bw as i32 * 2 + gap)) / 2;
    [Rect::new(x, y, bw, bh), Rect::new(x + bw as i32 + gap, y, bw, bh)]
}

/// No archive open: an invitation inside the list.
fn draw_empty(ctx: Ctx, s: &mut Surface, area: Rect) {
    let p = ctx.palette;
    let tile_side = ctx.px(56);
    let tile = Rect::new(area.x + (area.w as i32 - tile_side as i32) / 2, area.y + area.h as i32 / 2 - ctx.pxi(96), tile_side, tile_side);
    if !user_progs::icons::draw(s, tile, "archiver") {
        draw::rounded(s, tile, ctx.px(14), ctx.flat(p.acctint), 255);
        let g = ctx.px(32);
        iconset::draw(s, Fi::Compress, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, p.acc_ink, 255);
    }
    let line = Rect::new(area.x, tile.bottom() + ctx.px(8) as i32, area.w, u32::from(ctx.face(Role::Heading).line));
    paint::text_centered(ctx, s, Role::Heading, line, "Архиватор 7-Zip", p.ink);
    let sub = Rect::new(area.x, line.bottom() + ctx.px(2) as i32, area.w, u32::from(ctx.face(Role::Body).line));
    paint::text_centered(ctx, s, Role::Body, sub, "7z, zip, rar, tar, gz, xz, bz2, iso, cab и другие", p.ink3);
    let [open, create] = empty_buttons(ctx, area);
    paint::button(ctx, s, open, Weight::Primary, "Открыть архив…", false);
    paint::button(ctx, s, create, Weight::Normal, "Создать архив…", false);
}

fn form_head(ctx: Ctx, s: &mut Surface, head: Rect, title: &str, sub: &str) {
    let p = ctx.palette;
    let tile = Rect::new(head.x, head.y, ctx.px(44), ctx.px(44));
    draw::rounded(s, tile, ctx.px(10), ctx.flat(p.acctint), 255);
    draw::rounded_stroke(s, tile, ctx.px(10), p.accedge, 255);
    let g = ctx.px(24);
    iconset::draw(s, Fi::Compress, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, p.acc_ink, 255);
    let tx = tile.right() + ctx.px(14) as i32;
    let room = (head.right() - tx).max(0) as u32;
    paint::text_clipped(ctx, s, Role::Title, tx, tile.y, room, title, p.ink);
    paint::text_clipped(ctx, s, Role::Caption, tx, tile.y + i32::from(ctx.face(Role::Title).line) + ctx.px(4) as i32, room, sub, p.ink3);
}

fn label(ctx: Ctx, s: &mut Surface, field: Rect, text: &str) {
    let y = field.y - i32::from(ctx.face(Role::Caption).line) - ctx.px(6) as i32;
    paint::text(ctx, s, Role::Caption, field.x, y, text, ctx.palette.ink3);
}

fn field_box(ctx: Ctx, s: &mut Surface, rect: Rect, focused: bool) -> Rect {
    let p = ctx.palette;
    paint::sunk(ctx, s, rect, ctx.px(theme::R_ROW));
    if focused {
        draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.acc, 255);
    }
    Rect::new(rect.x + ctx.px(10) as i32, rect.y, rect.w.saturating_sub(ctx.px(20)), rect.h)
}

/// Ряд «таблеток» выбора; ответ — какая под точкой.
fn chip_rects<'a>(ctx: Ctx, row: Rect, labels: impl Iterator<Item = &'a str>) -> Vec<Rect> {
    let face = ctx.face(Role::Body);
    let mut x = row.x;
    let mut out = Vec::new();
    for label in labels {
        let w = face.width(label) + ctx.px(26);
        out.push(Rect::new(x, row.y, w, row.h));
        x += w as i32 + ctx.px(6) as i32;
    }
    out
}

fn chip_at<'a>(ctx: Ctx, row: Rect, labels: impl Iterator<Item = &'a str>, x: i32, y: i32) -> Option<usize> {
    chip_rects(ctx, row, labels).iter().position(|rect| rect.contains(x, y))
}

fn draw_chips<'a>(ctx: Ctx, s: &mut Surface, row: Rect, labels: impl Iterator<Item = &'a str> + Clone, chosen: usize, focused: bool, enabled: bool) {
    let p = ctx.palette;
    for (index, (rect, label)) in chip_rects(ctx, row, labels.clone()).iter().zip(labels).enumerate() {
        let on = index == chosen && enabled;
        if on {
            paint::button(ctx, s, *rect, Weight::Primary, label, false);
            if focused {
                let ring = ctx.px(2);
                draw::rounded_stroke(s, Rect::new(rect.x - ring as i32, rect.y - ring as i32, rect.w + ring * 2, rect.h + ring * 2), ctx.px(theme::R_ROW) + ring, p.accedge, 255);
            }
        } else if enabled {
            paint::button(ctx, s, *rect, Weight::Normal, label, false);
        } else {
            paint::button(ctx, s, *rect, Weight::Ghost, label, false);
        }
    }
}

fn draw_add(ctx: Ctx, s: &mut Surface, area: Rect, form: &AddForm) {
    let p = ctx.palette;
    let plan = FormPlan::new(ctx, area, 4);
    let what = match form.sources.len() {
        0 => String::from("Новый архив"),
        1 => format!("«{}»", file_name(&form.sources[0])),
        n => format!("{n} объект(ов) из «{}»", file_name(&parent(&form.sources[0]))),
    };
    form_head(ctx, s, plan.head, "Добавить в архив", &what);
    label(ctx, s, plan.fields[0], "Архив");
    let inner = field_box(ctx, s, plan.fields[0], form.focus == AddFocus::Name);
    form.name.draw(ctx, s, inner, form.focus == AddFocus::Name, p.ink);
    paint::button(ctx, s, plan.browse, Weight::Normal, "Обзор…", false);
    label(ctx, s, plan.fields[1], "Формат архива");
    draw_chips(ctx, s, plan.fields[1], FORMATS.iter().map(|format| format.0), form.format, form.focus == AddFocus::Format, true);
    let (_, _, _, levels, passwords, _) = FORMATS[form.format];
    label(ctx, s, plan.fields[2], "Уровень сжатия");
    draw_chips(ctx, s, plan.fields[2], LEVELS.iter().map(|level| level.0), form.level, form.focus == AddFocus::Level, levels);
    label(ctx, s, plan.fields[3], if passwords { "Пароль (необязательно; у 7z шифруются и имена файлов)" } else { "Пароль — только у 7z и zip" });
    let inner = field_box(ctx, s, plan.fields[3], form.focus == AddFocus::Password);
    let stars: String = "•".repeat(form.password.chars().count());
    Line::new(&stars).draw(ctx, s, inner, form.focus == AddFocus::Password && passwords, if passwords { p.ink } else { p.ink4 });
    if let Some(message) = &form.message {
        paint::text_clipped(ctx, s, Role::Caption, plan.message.x, paint::baseline(ctx, Role::Caption, plan.message), plan.message.w, message, p.bad_ink);
    }
    paint::button(ctx, s, plan.ok, Weight::Primary, "ОК", false);
    paint::button(ctx, s, plan.cancel, Weight::Normal, "Отмена", false);
}

fn show_box(ctx: Ctx, plan: &FormPlan) -> Rect {
    Rect::new(plan.fields[0].x, plan.fields[0].bottom() + ctx.px(16) as i32, plan.fields[0].w, ctx.px(26))
}

fn draw_extract(ctx: Ctx, s: &mut Surface, area: Rect, form: &ExtractForm) {
    let p = ctx.palette;
    let plan = FormPlan::new(ctx, area, 1);
    form_head(ctx, s, plan.head, "Извлечь все файлы", &format!("из «{}»", file_name(&form.archive)));
    label(ctx, s, plan.fields[0], "Файлы будут извлечены в эту папку:");
    let inner = field_box(ctx, s, plan.fields[0], form.focus == ExtractFocus::Dest);
    form.dest.draw(ctx, s, inner, form.focus == ExtractFocus::Dest, p.ink);
    paint::button(ctx, s, plan.browse, Weight::Normal, "Обзор…", false);
    let row = show_box(ctx, &plan);
    let b = ctx.px(18);
    let bx = Rect::new(row.x, row.y + (row.h - b) as i32 / 2, b, b);
    if form.show {
        draw::rounded(s, bx, ctx.px(4), p.acc, 255);
        iconset::draw(s, Fi::Check, bx.x + ctx.px(2) as i32, bx.y + ctx.px(2) as i32, b - ctx.px(4), mini_ui::Color::rgb(0xFF, 0xFF, 0xFF), 255);
    } else {
        draw::rounded_stroke(s, bx, ctx.px(4), p.ink3, 255);
    }
    if form.focus == ExtractFocus::Show {
        draw::rounded_stroke(s, Rect::new(bx.x - 3, bx.y - 3, bx.w + 6, bx.h + 6), ctx.px(6), p.acc, 255);
    }
    paint::text_clipped(ctx, s, Role::Body, bx.right() + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, row), row.w.saturating_sub(b + ctx.px(10)), "Показать извлечённые файлы", p.ink);
    if let Some(message) = &form.message {
        let m = Rect::new(row.x, row.bottom() + ctx.px(8) as i32, row.w, ctx.px(24));
        paint::text_clipped(ctx, s, Role::Caption, m.x, paint::baseline(ctx, Role::Caption, m), m.w, message, p.bad_ink);
    }
    paint::button(ctx, s, plan.ok, Weight::Primary, "Извлечь", false);
    paint::button(ctx, s, plan.cancel, Weight::Normal, "Отмена", false);
}

fn draw_progress(ctx: Ctx, s: &mut Surface, area: Rect, job: &Job) {
    let p = ctx.palette;
    draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 60);
    let card = progress_card(ctx, area);
    let inner = kit::card(ctx, s, card);
    let pad = ctx.px(20) as i32;
    let title = match &job.kind {
        JobKind::List => String::from("Чтение архива…"),
        JobKind::Extract { .. } => String::from("Извлечение…"),
        JobKind::Test => String::from("Проверка архива…"),
        JobKind::Add { .. } => String::from("Сжатие…"),
        JobKind::Delete => String::from("Удаление из архива…"),
        JobKind::Hash { .. } => String::from("Подсчёт контрольных сумм…"),
    };
    let room = card.w.saturating_sub(pad as u32 * 2);
    let mut y = card.y + pad;
    paint::text_clipped(inner, s, Role::Title, card.x + pad, y, room, &title, p.ink);
    y += i32::from(ctx.face(Role::Title).line) + ctx.px(6) as i32;
    let what = if job.current.is_empty() { file_name(&job.archive).to_string() } else { job.current.clone() };
    paint::text_clipped(inner, s, Role::Caption, card.x + pad, y, room, &what, p.ink3);
    y += i32::from(ctx.face(Role::Caption).line) + ctx.px(12) as i32;
    let bar = Rect::new(card.x + pad, y, room, ctx.px(8));
    paint::progress(inner, s, bar, job.percent, 100, Tone::Accent);
    y = bar.bottom() + ctx.px(8) as i32;
    let seconds = monotonic_ms().saturating_sub(job.started) / 1000;
    paint::text(inner, s, Role::Caption, card.x + pad, y, &format!("{} %   ·   {}:{:02}", job.percent, seconds / 60, seconds % 60), p.ink4);
    let [_, cancel] = card_buttons(ctx, card);
    paint::button(inner, s, cancel, Weight::Normal, "Отмена", false);
}

fn password_field(ctx: Ctx, card: Rect) -> Rect {
    let pad = ctx.px(20) as i32;
    Rect::new(card.x + pad, card.y + pad + ctx.px(56) as i32, card.w.saturating_sub(pad as u32 * 2), ctx.px(34))
}

fn draw_password(ctx: Ctx, s: &mut Surface, area: Rect, line: &Line, ask: &Ask) {
    let p = ctx.palette;
    draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 70);
    let card = ask_card(ctx, area);
    let inner = kit::card(ctx, s, card);
    let pad = ctx.px(20) as i32;
    let g = ctx.px(20);
    iconset::draw(s, Fi::Lock, card.x + pad, card.y + pad, g, p.acc, 255);
    paint::text_clipped(inner, s, Role::Title, card.x + pad + g as i32 + ctx.px(10) as i32, card.y + pad, card.w.saturating_sub(pad as u32 * 2 + g + ctx.px(10)), "Введите пароль", p.ink);
    paint::text_clipped(inner, s, Role::Caption, card.x + pad, card.y + pad + ctx.px(30) as i32, card.w.saturating_sub(pad as u32 * 2), &ask.text, p.ink3);
    let field = password_field(ctx, card);
    let text_rect = field_box(inner, s, field, true);
    let stars: String = "•".repeat(line.text.chars().count());
    Line::new(&stars).draw(inner, s, text_rect, true, p.ink);
    let [ok, cancel] = card_buttons(ctx, card);
    paint::button(inner, s, ok, Weight::Primary, "ОК", false);
    paint::button(inner, s, cancel, Weight::Normal, "Отмена", false);
}

fn draw_message(ctx: Ctx, s: &mut Surface, area: Rect, title: &str, text: &str, bad: bool) {
    let p = ctx.palette;
    draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 70);
    let card = ask_card(ctx, area);
    let inner = kit::card(ctx, s, card);
    let pad = ctx.px(20) as i32;
    let g = ctx.px(22);
    iconset::draw(s, if bad { Fi::Warning } else { Fi::ShieldCheck }, card.x + pad, card.y + pad, g, if bad { p.bad_ink } else { p.ok_ink }, 255);
    let tx = card.x + pad + g as i32 + ctx.px(10) as i32;
    let room = (card.right() - pad - tx).max(0) as u32;
    paint::text_clipped(inner, s, Role::Title, tx, card.y + pad, room, title, p.ink);
    let mut y = card.y + pad + i32::from(ctx.face(Role::Title).line) + ctx.px(8) as i32;
    let face = ctx.face(Role::Body);
    let mut current = String::new();
    let mut lines = 0;
    for word in text.split(' ') {
        let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
        if face.width(&candidate) > room && !current.is_empty() {
            paint::text_clipped(inner, s, Role::Body, tx, y, room, &current, p.ink2);
            y += i32::from(face.line);
            lines += 1;
            current = word.to_string();
            if lines >= 3 {
                break;
            }
        } else {
            current = candidate;
        }
    }
    if lines < 3 && !current.is_empty() {
        paint::text_clipped(inner, s, Role::Body, tx, y, room, &current, p.ink2);
    }
    let [_, close_button] = card_buttons(ctx, card);
    paint::button(inner, s, close_button, Weight::Primary, "ОК", false);
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

/// Пути из аргументов: `%20` → пробел, `@файл` — список строками; без `%20`
/// путь с пробелом приезжает словами — тогда это один путь.
fn paths_of(args: &Args, from: usize) -> Vec<String> {
    let words: Vec<String> = (from..args.len()).filter_map(|index| args.get(index)).map(String::from).collect();
    if let [single] = words.as_slice() {
        if let Some(list) = single.strip_prefix('@') {
            let text = read_text(list);
            let _ = user_progs::remove(list);
            return text.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect();
        }
    }
    let decoded: Vec<String> = words.iter().map(|word| word.replace("%20", " ").replace("%25", "%")).collect();
    // Все слова — абсолютные пути: это список. Иначе — один путь с пробелами.
    if decoded.iter().all(|word| word.starts_with('/')) {
        decoded
    } else {
        alloc::vec![decoded.join(" ")]
    }
}

fn read_text(path: &str) -> String {
    let fd = user_progs::open(path);
    if fd < 0 {
        return String::new();
    }
    let mut out = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let got = read(fd, &mut buffer);
        if got <= 0 || out.len() > (1 << 20) {
            break;
        }
        out.extend_from_slice(&buffer[..got as usize]);
    }
    close(fd);
    String::from_utf8_lossy(&out).into_owned()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно такими, какими их положило ядро.
    let args = unsafe { Args::new(argc, argv) };
    let command = args.get(1).unwrap_or("").to_string();
    let info = app::start(&SPEC);
    let scale = app::ui_scale(&info);
    let mut archiver = Archiver::new();
    let mut size = app::window_size(&info, (1100, 640));
    match command.as_str() {
        "--add" => {
            let sources = paths_of(&args, 2);
            archiver.start_add_form(sources);
            size = ((560u32 * scale).min(info.screen_w), (470u32 * scale).min(info.screen_h));
        }
        // «Добавить к "имя.7z"» / «Добавить к "имя.zip"» of the 7-Zip menu
        // (25.11): next to the first, named as 7-Zip names it, no questions.
        "--7z" | "--zip" => {
            let sources = paths_of(&args, 2);
            if !sources.is_empty() {
                let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
                let (ext, format) = if command == "--7z" { ("7z", 0) } else { ("zip", 1) };
                let name = sysconf::filetypes::add_to_target(&refs, sources.len() == 1 && is_dir(&sources[0]), ext);
                println(&format!("archiver: adding {} item(s) to '{name}'", sources.len()));
                archiver.quit_after = true;
                archiver.add_files(&name, &sources, format, 2, None);
            }
            size = ((500u32 * scale).min(info.screen_w), (260u32 * scale).min(info.screen_h));
        }
        // «Распаковать здесь», «Распаковать в "имя\"», «Тестировать» — of
        // one or several archives, one after another.
        "--extract-here" | "--extract-to" | "--test" => {
            let mut archives = paths_of(&args, 2);
            if !archives.is_empty() {
                let first = archives.remove(0);
                archiver.queue = archives;
                archiver.archive = first.clone();
                archiver.quit_after = true;
                match command.as_str() {
                    "--extract-here" => archiver.extract_here(&first),
                    "--extract-to" => {
                        archiver.queue_kind = Queue::To;
                        archiver.extract_to_folder(&first);
                    }
                    _ => {
                        archiver.queue_kind = Queue::Test;
                        archiver.test_archive(&first);
                    }
                }
            }
            size = ((500u32 * scale).min(info.screen_w), (260u32 * scale).min(info.screen_h));
        }
        // «CRC SHA ▸ метод» (25.11): `--hash CRC32|CRC64|SHA1|SHA256|* ПУТЬ…`.
        "--hash" => {
            let method = args.get(2).unwrap_or("CRC32").to_string();
            let paths = paths_of(&args, 3);
            if !paths.is_empty() {
                archiver.quit_after = true;
                archiver.hash(&paths, &method);
            }
            size = ((680u32 * scale).min(info.screen_w), (360u32 * scale).min(info.screen_h));
        }
        // «Открыть архив ▸ тип» (25.11): `--type 7z АРХИВ`.
        "--type" => {
            let kind = args.get(2).unwrap_or("*").to_string();
            let archive = paths_of(&args, 3).into_iter().next().unwrap_or_default();
            if !archive.is_empty() {
                archiver.type_arg = format!("-t{kind}");
                println(&format!("archiver: opening '{archive}' as type {kind}"));
                archiver.open_archive(&archive);
            }
        }
        "--extract" => {
            let archive = paths_of(&args, 2).into_iter().next().unwrap_or_default();
            let dir = parent(&archive);
            let dest = free_name(&dir, &stem(file_name(&archive)), "");
            println(&format!("archiver: extract form for '{archive}' to '{dest}'"));
            archiver.mode = Mode::Extract(ExtractForm { archive, dest: Line::new(&dest), show: true, focus: ExtractFocus::Dest, message: None });
            size = ((560u32 * scale).min(info.screen_w), (300u32 * scale).min(info.screen_h));
        }
        "" => {}
        _ => {
            let archive = paths_of(&args, 1).into_iter().next().unwrap_or_default();
            if !archive.is_empty() {
                archiver.open_archive(&archive);
            }
        }
    }
    app::run(&SPEC, &info, size, move |_, _| archiver)
}
