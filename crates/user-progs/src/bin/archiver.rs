//! «Архиватор 7-Zip» (24.11, Роман: 7-Zip как стандартная программа) — окно
//! над консольным `7zz` (`/bin/7zz`, 7-Zip 26.06 из исходников 7-zip.org,
//! `xtask/src/sevenzip.rs`).
//!
//! ```text
//! archiver                         пустое окно: «Открыть архив…», «Создать архив…»
//! archiver АРХИВ                   содержимое архива: папки, извлечь, проверить, добавить
//! archiver --extract АРХИВ         «Извлечь все…»: куда, затем ход и закрытие
//! archiver --extract-here АРХИВ…   «Извлечь сюда»: рядом с архивом, без вопросов
//! archiver --add ПУТЬ…             «Добавить в архив…»: имя, формат, сжатие, пароль
//! archiver --zip ПУТЬ…             «Сжать в ZIP»: ИМЯ.zip рядом, без вопросов
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
}

/// Сведения об архиве целиком.
#[derive(Default)]
struct Info {
    kind: String,
    method: String,
    physical: u64,
    solid: bool,
}

/// Строка списка текущей папки архива.
struct Row {
    name: String,
    folder: bool,
    size: u64,
    packed: u64,
    modified: String,
    encrypted: bool,
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
            }
            "Encrypted" => item.encrypted = value.trim() == "+",
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
        match rows.iter_mut().find(|row| row.name == name && row.folder == folder) {
            Some(row) => {
                if deeper && !item.folder {
                    row.size += item.size;
                    row.packed += item.packed;
                }
                row.encrypted |= item.encrypted;
                if !deeper && !item.modified.is_empty() {
                    row.modified = item.modified.clone();
                }
            }
            None => rows.push(Row {
                name: name.to_string(),
                folder,
                size: if deeper && item.folder { 0 } else { item.size },
                packed: if deeper && item.folder { 0 } else { item.packed },
                modified: if deeper { String::new() } else { item.modified.clone() },
                encrypted: item.encrypted,
            }),
        }
    }
    rows.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
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
    Extract { dest: String, open: Option<String>, show: bool },
    Test,
    Add { archive: String },
    Delete,
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
}

/// Вопрос посреди окна.
enum Overlay {
    None,
    Password(Line, Ask),
    /// Сообщение: заголовок, текст, ошибка ли.
    Message(String, String, bool),
    /// Удалить выбранное из архива? (0 — «Удалить», 1 — «Отмена».)
    Delete(usize),
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
    done: bool,
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    toolbar: Rect,
    buttons: [Rect; 5],
    crumbs: Rect,
    up: Rect,
    table: Table,
    status: Rect,
}

const TOOL_LABELS: [&str; 5] = ["Извлечь…", "Проверить", "Добавить…", "Удалить", "Открыть…"];

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let mut x = frame.toolbar.x + pad;
        let mut buttons = [Rect::EMPTY; 5];
        for (index, label) in TOOL_LABELS.iter().enumerate() {
            let w = face.width(label) + ctx.px(28 + 22);
            if index == 4 {
                // «Открыть…» — у правого края.
                buttons[index] = Rect::new(frame.toolbar.right() - pad - w as i32, by, w, bh);
            } else {
                buttons[index] = Rect::new(x, by, w, bh);
                x += w as i32 + ctx.px(6) as i32;
            }
        }
        let crumb_h = ctx.px(36);
        let crumb_row = Rect::new(frame.body.x, frame.body.y, frame.body.w, crumb_h);
        let up = Rect::new(crumb_row.x + pad, crumb_row.y + ctx.px(4) as i32, ctx.px(28), ctx.px(28));
        let crumbs = Rect::new(up.right() + ctx.px(8) as i32, crumb_row.y + ctx.px(4) as i32, (crumb_row.right() - pad - up.right() - ctx.px(8) as i32).max(0) as u32, ctx.px(28));
        let body = Rect::new(frame.body.x, crumb_row.bottom(), frame.body.w, frame.body.h.saturating_sub(crumb_h));
        Self { toolbar: frame.toolbar, buttons, crumbs, up, table: Table::new(ctx, body), status: frame.status }
    }

    fn step(&self) -> u32 {
        self.table.row_h
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
            done: false,
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
        self.run(JobKind::Extract { dest: dest.to_string(), open, show }, &archive, &args, password.as_deref());
    }

    fn extract_here(&mut self, archive: &str) {
        let dir = parent(archive);
        // Как «Извлечь сюда» 7-Zip: рядом с архивом; совпавшие имена не
        // затираются — новые получают «_1» (`-aou`).
        let args = format!("x -y -aou -bso1 -bse1 -bsp1 -sccUTF-8 {} {}", quoted(&format!("-o{dir}")), quoted(archive));
        let password = self.password.clone();
        self.run(JobKind::Extract { dest: dir, open: None, show: false }, archive, &args, password.as_deref());
    }

    fn test(&mut self) {
        if self.archive.is_empty() {
            return;
        }
        let args = format!("t -bso1 -bse1 -bsp1 -sccUTF-8 {}", quoted(&self.archive));
        let password = self.password.clone();
        let archive = self.archive.clone();
        self.run(JobKind::Test, &archive, &args, password.as_deref());
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
        let names: Vec<String> = self.chosen().into_iter().filter_map(|index| self.rows.get(index)).map(|row| self.inner_path(row)).collect();
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
            JobKind::Extract { dest, open, show } => {
                if !ok {
                    let why = error_lines(&text);
                    println(&format!("archiver: extract to '{dest}' failed: {why}"));
                    self.overlay = Overlay::Message(String::from("Ошибка при извлечении"), why, true);
                    self.quit_after = false;
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
                if let Some(next) = (!self.queue.is_empty()).then(|| self.queue.remove(0)) {
                    self.extract_here(&next);
                }
            }
            JobKind::Test => {
                if ok {
                    println(&format!("archiver: test of '{}' ok", job.archive));
                    self.overlay = Overlay::Message(String::from("Ошибок не найдено"), format!("Архив «{}» проверен: все файлы целы.", file_name(&job.archive)), false);
                } else {
                    let why = error_lines(&text);
                    println(&format!("archiver: test of '{}' failed: {why}", job.archive));
                    self.overlay = Overlay::Message(String::from("Архив повреждён"), why, true);
                }
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
            Mode::Browse => app::SizeLimits::min(560, 360),
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
            Overlay::Message(..) => {
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
        if ctrl {
            match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                Some('o') => self.pick(Pick::OpenArchive),
                Some('e') if !self.archive.is_empty() => self.pick(Pick::ExtractTo),
                Some('t') => self.test(),
                Some('a') => {
                    self.picked = alloc::vec![true; self.rows.len()];
                }
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
        if self.archive.is_empty() {
            let [open, create] = empty_buttons(ctx, area);
            if open.contains(x, y) {
                self.pick(Pick::OpenArchive);
            } else if create.contains(x, y) {
                self.start_add_form(Vec::new());
            }
            return true;
        }
        let plan = Plan::new(ctx, area);
        if let Some(index) = plan.buttons.iter().position(|rect| rect.contains(x, y)) {
            match index {
                0 => self.pick(Pick::ExtractTo),
                1 => self.test(),
                2 => self.pick(Pick::AddFiles),
                3 if !self.rows.is_empty() => self.overlay = Overlay::Delete(1),
                4 => self.pick(Pick::OpenArchive),
                _ => {}
            }
            return true;
        }
        if plan.up.contains(x, y) {
            return self.go_up();
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
            Mode::Browse if self.archive.is_empty() && self.job.is_none() => draw_empty(ctx, s, area),
            Mode::Browse => self.draw_browser(ctx, s, area),
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
        }
    }
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
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        let icons = [Fi::FolderOpen, Fi::ShieldCheck, Fi::Plus, Fi::Trash, Fi::Compress];
        for (index, (rect, label)) in plan.buttons.iter().zip(TOOL_LABELS).enumerate() {
            let weight = if index == 0 { Weight::Primary } else { Weight::Normal };
            paint::button(bar, s, *rect, weight, "", false);
            let g = ctx.px(16);
            let ink = if index == 0 { mini_ui::Color::rgb(0xFF, 0xFF, 0xFF) } else { p.ink2 };
            iconset::draw(s, icons[index], rect.x + ctx.px(12) as i32, rect.y + (rect.h - g) as i32 / 2, g, ink, 255);
            paint::text(bar, s, if index == 0 { Role::Label } else { Role::Body }, rect.x + ctx.px(34) as i32, paint::baseline(bar, Role::Body, *rect), label, ink);
        }

        // Путь внутри архива.
        paint::icon_button(ctx, s, plan.up, Icon::Up, Weight::Ghost, false);
        paint::sunk(ctx, s, plan.crumbs, ctx.px(theme::R_ROW));
        // Архив › папка › папка — звенья, между ними шеврон.
        let g = ctx.px(16);
        iconset::draw(s, Fi::Compress, plan.crumbs.x + ctx.px(10) as i32, plan.crumbs.y + (plan.crumbs.h - g) as i32 / 2, g, p.acc, 255);
        let y = paint::baseline(ctx, Role::Body, plan.crumbs);
        let right = plan.crumbs.right() - ctx.px(10) as i32;
        let mut x = plan.crumbs.x + ctx.px(34) as i32;
        let parts: Vec<&str> = core::iter::once(file_name(&self.archive)).chain(self.dir.split('/').filter(|part| !part.is_empty())).collect();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                let c = ctx.px(12);
                glyphicon::draw(s, Icon::ChevronRight, x + ctx.px(4) as i32, plan.crumbs.y + (plan.crumbs.h - c) as i32 / 2, c, p.ink4, 255);
                x += (c + ctx.px(8)) as i32;
            }
            let room = (right - x).max(0) as u32;
            let ink = if index + 1 == parts.len() { p.ink } else { p.ink3 };
            paint::text_clipped(ctx, s, Role::Body, x, y, room, part, ink);
            x += ctx.face(Role::Body).width(part).min(room) as i32;
        }

        // Таблица.
        let table = plan.table;
        let columns = table.columns(ctx, &[0, ctx.px(110), ctx.px(110), ctx.px(150)], ctx.px(180));
        table.draw_header(ctx, s, &columns, &["Имя", "Размер", "Сжатый", "Изменён"]);
        let list = table.list;
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
            let name = if row.encrypted && !row.folder { format!("{} *", row.name) } else { row.name.clone() };
            kit::cell(ctx, s, columns[0], icon.right() + ctx.px(10) as i32, rect, Role::Body, &name, if state == RowState::Selected { p.ink } else { ink });
            let size = if row.folder && row.size == 0 { String::new() } else { size_text(row.size) };
            let packed = if row.folder || row.packed == 0 { String::new() } else { size_text(row.packed) };
            kit::cell(ctx, s, columns[1], columns[1].x, rect, Role::Body, &size, ink);
            kit::cell(ctx, s, columns[2], columns[2].x, rect, Role::Body, &packed, ink);
            kit::cell(ctx, s, columns[3], columns[3].x, rect, Role::Caption, &row.modified, ink);
        }
        if let Some(bar) = ScrollBar::vertical(ctx, list, content, &self.scroll) {
            bar.draw(ctx, s, &self.scroll);
        }
        if self.rows.is_empty() && self.job.is_none() {
            paint::text_centered(ctx, s, Role::Body, Rect::new(list.x, list.y, list.w, ctx.px(60)), "Папка пуста", p.ink4);
        }

        let picked = self.picked.iter().filter(|slot| **slot).count();
        let mut status = if picked > 0 { format!("Отмечено: {picked} из {}", self.rows.len()) } else { kit::items_text(self.rows.len()) };
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
    let bw = ctx.px(180);
    let bh = ctx.px(36);
    let y = area.y + area.h as i32 / 2 + ctx.px(40) as i32;
    let gap = ctx.px(12) as i32;
    let x = area.x + (area.w as i32 - (bw as i32 * 2 + gap)) / 2;
    [Rect::new(x, y, bw, bh), Rect::new(x + bw as i32 + gap, y, bw, bh)]
}

fn draw_empty(ctx: Ctx, s: &mut Surface, area: Rect) {
    let p = ctx.palette;
    let tile_side = ctx.px(72);
    let tile = Rect::new(area.x + (area.w as i32 - tile_side as i32) / 2, area.y + area.h as i32 / 2 - ctx.px(110) as i32, tile_side, tile_side);
    draw::rounded(s, tile, ctx.px(16), ctx.flat(p.acctint), 255);
    draw::rounded_stroke(s, tile, ctx.px(16), p.accedge, 255);
    let g = ctx.px(40);
    iconset::draw(s, Fi::Compress, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, p.acc_ink, 255);
    let line = Rect::new(area.x, tile.bottom() + ctx.px(14) as i32, area.w, u32::from(ctx.face(Role::Heading).line));
    paint::text_centered(ctx, s, Role::Heading, line, "Архиватор 7-Zip", p.ink);
    let sub = Rect::new(area.x, line.bottom() + ctx.px(4) as i32, area.w, u32::from(ctx.face(Role::Body).line));
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
    let mut size = app::window_size(&info, (860, 560));
    match command.as_str() {
        "--add" => {
            let sources = paths_of(&args, 2);
            archiver.start_add_form(sources);
            size = ((560u32 * scale).min(info.screen_w), (470u32 * scale).min(info.screen_h));
        }
        "--zip" => {
            let sources = paths_of(&args, 2);
            if let Some(first) = sources.first() {
                let dir = parent(first);
                let base = if sources.len() == 1 { stem(file_name(first)) } else { file_name(&dir).to_string() };
                let name = free_name(&dir, if base.is_empty() || base == "/" { "Архив" } else { &base }, ".zip");
                archiver.quit_after = true;
                archiver.add_files(&name, &sources, 1, 2, None);
            }
            size = ((500u32 * scale).min(info.screen_w), (260u32 * scale).min(info.screen_h));
        }
        "--extract-here" => {
            let mut archives = paths_of(&args, 2);
            if !archives.is_empty() {
                let first = archives.remove(0);
                archiver.queue = archives;
                archiver.archive = first.clone();
                archiver.quit_after = true;
                archiver.extract_here(&first);
            }
            size = ((500u32 * scale).min(info.screen_w), (260u32 * scale).min(info.screen_h));
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
