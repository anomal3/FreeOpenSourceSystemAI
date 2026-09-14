//! Двухпанельный файловый менеджер в духе Far и Midnight Commander.
//!
//! # Что эта программа доказывает
//!
//! Что системных вызовов хватает для приложения, которое читает клавиатуру,
//! рисует весь экран и работает с файлами, — и что для этого не нужно ни строчки
//! в ядре. `mc` здесь обычная программа из `/bin`: те же системные вызовы, что у
//! `hello`, тот же ELF, то же адресное пространство.
//!
//! # Как выглядит и почему так (фаза С9)
//!
//! Как Far: синие панели в двойной рамке, имя и размер столбцами, путь в
//! заголовке, строка о файле под курсором, внизу — строка сообщений и полоса
//! F-клавиш с подписями. Вопросы — серыми окнами с кнопками поверх панелей, а не
//! строкой «rename to:» внизу экрана. Первая версия рисовала список инверсией,
//! которой терминал не понимал, и выглядела сырым выводом `ls`; человеку,
//! видевшему Far или mc, нужно было узнать раскладку, а не выучить новую.
//!
//! Это **не** Far и не Midnight Commander: нет выделения нескольких файлов,
//! копирования папок, командной строки, архивов и сети. Что не сделано — названо
//! в подсказках и в README, а не спрятано.
//!
//! # Как рисует
//!
//! Кадр собирается на холсте ячеек ([`Canvas`]) и уходит в терминал **одной
//! записью**, причём только строки, изменившиеся с прошлого кадра. Прежняя
//! версия печатала по символу: каждый `write` в ядре — это разбор, перерисовка
//! стола и копия в серийную линию, и экран 100×40 стоил четыре тысячи таких
//! проходов на одно нажатие стрелки. Цвета — номерами палитры в 256 цветов:
//! тёмно-синего Far среди шестнадцати основных нет.
//!
//! Пока в очереди ввода есть клавиши, кадр не выводится вовсе: сначала
//! разбирается всё набранное, потом показывается итог. Иначе быстрый набор в
//! редакторе упирался бы в скорость рисования, и очередь терминала на 256 байт
//! теряла бы буквы — тот же дефект, что был у «Параметров» (очередь клавиш за
//! медленным кадром).
//!
//! # Почему диагностика идёт в дескриптор 2
//!
//! Потому что дескриптор 1 занят картинкой. Ядро отправляет второй дескриптор в
//! журнал, не трогая окно, — ровно затем, чтобы у программы с картинкой был
//! канал для слов. По этим строкам (`mc: copied …`, `mc: saved …`) стенд и
//! проверяет программу: снимок экрана доказательством не считается.
//!
//! # Память
//!
//! Куча программы — один мегабайт на всё, поэтому пределы названы: просмотр
//! читает не больше [`VIEW_MAX`], правка открывает файлы до [`EDIT_MAX`], панель
//! держит до [`MAX_ENTRIES`] записей. Упёршись в предел, программа **говорит**
//! об этом — молчаливое обрезание списка файлов это потерянные файлы.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use user_abi::{ERR_EXISTS, ERR_NOT_EMPTY, ERR_UNSUPPORTED, Stat};
use user_progs::{
    Args, Dirent, ERR_NO_SPACE, ERR_NOT_FOUND, ERR_PERMISSION, FD_STDIN, KIND_DIRECTORY, POLL_IN,
    PollFd, close, error, exit, file_size, mkdir, open, open_write, poll, print, println, read,
    read_stdin, readdir, remove, rename, set_raw, stat, window_size, write,
};

/// Сколько записей держит панель.
const MAX_ENTRIES: usize = 2000;
/// Сколько байт файла читает просмотр.
const VIEW_MAX: usize = 256 * 1024;
/// Сколько строк просмотр размечает: указатель на строку — четыре байта, и
/// файл из одних переводов строки иначе съел бы половину кучи.
const VIEW_LINES_MAX: usize = 32_000;
/// Самый большой файл, который открывает правка.
const EDIT_MAX: usize = 64 * 1024;
/// Кусок при копировании.
const CHUNK: usize = 4096;
/// Сколько ждать продолжения после `ESC`, прежде чем решить, что это была сама
/// клавиша Escape.
///
/// Клавиатура присылает последовательность одной порцией, но серийная линия —
/// байтами, и хвост `ESC [ 1 5 ~` может приехать на несколько миллисекунд
/// позже. Шестьдесят — с запасом на линию и незаметно для руки.
const ESCAPE_WAIT_MS: i64 = 60;
/// Самое маленькое окно, в котором панели ещё рисуются.
const MIN_COLS: usize = 40;
const MIN_ROWS: usize = 10;
/// Самое длинное имя, которое принимает поле ввода.
const FIELD_MAX: usize = 255;

/// Номера цветов в палитре терминала на 256 цветов.
///
/// Подобраны по Far: панель `#000087`, рамка и файлы — оттенки голубого,
/// папки белые, заголовки столбцов жёлтые, курсор — чёрным по бирюзовому.
mod ink {
    pub const PANEL: u8 = 18;
    pub const FRAME: u8 = 45;
    pub const FILE: u8 = 123;
    pub const DIR: u8 = 231;
    pub const TITLE: u8 = 226;
    pub const CURSOR_FG: u8 = 16;
    pub const CURSOR_BG: u8 = 37;
    pub const BAR_NUM_FG: u8 = 252;
    pub const BAR_NUM_BG: u8 = 16;
    pub const BAR_FG: u8 = 16;
    pub const BAR_BG: u8 = 37;
    pub const LINE_FG: u8 = 250;
    pub const LINE_BG: u8 = 16;
    pub const DIALOG_FG: u8 = 16;
    pub const DIALOG_BG: u8 = 250;
    pub const DANGER_FG: u8 = 231;
    pub const DANGER_BG: u8 = 124;
    pub const FIELD_FG: u8 = 16;
    pub const FIELD_BG: u8 = 37;
    pub const PICKED_FG: u8 = 231;
    pub const PICKED_BG: u8 = 24;
    pub const SHADOW_FG: u8 = 244;
    pub const SHADOW_BG: u8 = 16;
}

/// Подписи F-клавиш на панелях. Пустая подпись — клавиша ничего не делает, и
/// это видно, а не угадывается.
const PANEL_KEYS: [&str; 10] =
    ["Помощь", "", "Просм", "Правка", "Копия", "Перен", "Папка", "Удал", "", "Выход"];
const VIEW_KEYS: [&str; 10] = ["", "", "Выход", "Байты", "", "", "", "", "", "Выход"];
const VIEW_HEX_KEYS: [&str; 10] = ["", "", "Выход", "Текст", "", "", "", "", "", "Выход"];
const EDIT_KEYS: [&str; 10] = ["", "Сохран", "", "", "", "", "", "", "", "Выход"];

/// Что показывает строка сообщений, пока сообщать нечего.
const HINT: &str = "Tab — другая панель · Enter — открыть · Ctrl+R — перечитать · F1 — помощь";

// ---------------------------------------------------------------------------
// Холст
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    fg: u8,
    bg: u8,
}

const BLANK: Cell = Cell { ch: ' ', fg: ink::LINE_FG, bg: ink::LINE_BG };

/// Экран в ячейках: что нарисовано сейчас и что уже показано.
struct Canvas {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    /// Что сейчас на экране терминала — по нему решается, какие строки слать.
    shown: Vec<Cell>,
    /// Экран не совпадает с `shown` (первый кадр, новый размер): слать всё.
    fresh: bool,
    /// Где показать курсор; `None` — спрятать.
    cursor: Option<(usize, usize)>,
    shown_cursor: Option<(usize, usize)>,
    out: String,
}

impl Canvas {
    const fn new() -> Self {
        Self {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            shown: Vec::new(),
            fresh: true,
            cursor: None,
            shown_cursor: None,
            out: String::new(),
        }
    }

    /// Подогнать холст под окно. Окно человек вправе растянуть в любой момент.
    fn fit_window(&mut self) {
        let (cols, rows) = window_cells();
        if cols == self.cols && rows == self.rows {
            return;
        }
        let Some(len) = cols.checked_mul(rows) else {
            return;
        };
        let mut cells = Vec::new();
        let mut shown = Vec::new();
        if cells.try_reserve_exact(len).is_err() || shown.try_reserve_exact(len).is_err() {
            // Памяти на новый размер нет — остаёмся в старом: картинка обрежется
            // или не заполнит окно, но программа останется жива.
            return;
        }
        cells.resize(len, BLANK);
        shown.resize(len, BLANK);
        self.cols = cols;
        self.rows = rows;
        self.cells = cells;
        self.shown = shown;
        self.fresh = true;
        // Размер называется в журнале: стенд проверяет по этой строке, что `mc`
        // пошёл за окном, — снимок экрана доказательством не считается.
        error(&format!("mc: window is {cols}x{rows}\n"));
    }

    fn put(&mut self, row: usize, col: usize, ch: char, fg: u8, bg: u8) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        // Управляющий знак в имени файла ушёл бы в терминал командой: `ESC` в
        // имени перекрасил бы полэкрана.
        let ch = if ch.is_control() { '?' } else { ch };
        self.cells[row * self.cols + col] = Cell { ch, fg, bg };
    }

    fn fill(&mut self, row: usize, col: usize, width: usize, height: usize, ch: char, fg: u8, bg: u8) {
        for r in row..row.saturating_add(height) {
            for c in col..col.saturating_add(width) {
                self.put(r, c, ch, fg, bg);
            }
        }
    }

    /// Строка ровно в `width` ячеек: короче — дополняется пробелами, длиннее —
    /// обрезается многоточием, чтобы обрезанное не читалось как целое.
    fn text(&mut self, row: usize, col: usize, width: usize, text: &str, fg: u8, bg: u8) {
        let count = text.chars().count();
        let mut x = 0;
        if count > width && width >= 2 {
            for ch in text.chars().take(width - 1) {
                self.put(row, col + x, ch, fg, bg);
                x += 1;
            }
            self.put(row, col + x, '…', fg, bg);
            x += 1;
        } else {
            for ch in text.chars().take(width) {
                self.put(row, col + x, ch, fg, bg);
                x += 1;
            }
        }
        while x < width {
            self.put(row, col + x, ' ', fg, bg);
            x += 1;
        }
    }

    fn centered(&mut self, row: usize, col: usize, width: usize, text: &str, fg: u8, bg: u8) {
        let n = text.chars().count().min(width);
        self.text(row, col + (width - n) / 2, n, text, fg, bg);
    }

    fn right(&mut self, row: usize, col: usize, width: usize, text: &str, fg: u8, bg: u8) {
        let n = text.chars().count().min(width);
        self.text(row, col + width - n, n, text, fg, bg);
    }

    fn double_box(&mut self, row: usize, col: usize, width: usize, height: usize, fg: u8, bg: u8) {
        if width < 2 || height < 2 {
            return;
        }
        let right = col + width - 1;
        let bottom = row + height - 1;
        self.put(row, col, '╔', fg, bg);
        self.fill(row, col + 1, width - 2, 1, '═', fg, bg);
        self.put(row, right, '╗', fg, bg);
        for r in row + 1..bottom {
            self.put(r, col, '║', fg, bg);
            self.put(r, right, '║', fg, bg);
        }
        self.put(bottom, col, '╚', fg, bg);
        self.fill(bottom, col + 1, width - 2, 1, '═', fg, bg);
        self.put(bottom, right, '╝', fg, bg);
    }

    /// Тень окна: знаки под ней остаются, но гаснут.
    fn shade(&mut self, row: usize, col: usize, width: usize, height: usize) {
        for r in row..row.saturating_add(height).min(self.rows) {
            for c in col..col.saturating_add(width).min(self.cols) {
                let cell = &mut self.cells[r * self.cols + c];
                cell.fg = ink::SHADOW_FG;
                cell.bg = ink::SHADOW_BG;
            }
        }
    }

    /// Отправить изменившееся одной записью.
    fn present(&mut self) {
        self.out.clear();
        let mut changed = false;
        for row in 0..self.rows {
            let start = row * self.cols;
            let line = &self.cells[start..start + self.cols];
            if !self.fresh && line == &self.shown[start..start + self.cols] {
                continue;
            }
            changed = true;
            // Строка шлётся целиком и начинается с установки курсора и цвета:
            // так каждая строка самодостаточна, и пропуск соседней её не портит.
            let _ = write!(self.out, "\x1b[{};1H", row + 1);
            let mut colors = None;
            for cell in line {
                if colors != Some((cell.fg, cell.bg)) {
                    let _ = write!(self.out, "\x1b[38;5;{};48;5;{}m", cell.fg, cell.bg);
                    colors = Some((cell.fg, cell.bg));
                }
                self.out.push(cell.ch);
            }
        }
        if !changed && !self.fresh && self.cursor == self.shown_cursor {
            return;
        }
        match self.cursor {
            Some((row, col)) => {
                let _ = write!(self.out, "\x1b[{};{}H\x1b[?25h", row + 1, col + 1);
            }
            None => self.out.push_str("\x1b[?25l"),
        }
        write(1, self.out.as_bytes());
        self.shown.copy_from_slice(&self.cells);
        self.shown_cursor = self.cursor;
        self.fresh = false;
    }
}

/// Полоса F-клавиш: номер светлым по чёрному, подпись чёрным по бирюзовому.
fn keybar(canvas: &mut Canvas, row: usize, labels: &[&str; 10]) {
    let cols = canvas.cols;
    let slot = cols / 10;
    for (index, label) in labels.iter().enumerate() {
        let x = index * slot;
        let width = if index == 9 { cols - x } else { slot };
        let number = index + 1;
        let digits = if number == 10 { 2 } else { 1 };
        let mut buffer = [0u8; 2];
        let number_text = if number == 10 {
            "10"
        } else {
            buffer[0] = b'0' + number as u8;
            core::str::from_utf8(&buffer[..1]).unwrap_or("?")
        };
        canvas.text(row, x, digits.min(width), number_text, ink::BAR_NUM_FG, ink::BAR_NUM_BG);
        if width > digits {
            canvas.text(row, x + digits, width - digits, label, ink::BAR_FG, ink::BAR_BG);
        }
    }
}

// ---------------------------------------------------------------------------
// Ввод
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Enter,
    Tab,
    Backspace,
    Escape,
    F(u8),
    Char(char),
    /// Ctrl с буквой: `Ctrl(b'r')`.
    Ctrl(u8),
    Unknown,
    /// Окно терминала изменило размер — клавиши не было, но перерисовать надо.
    Resize,
    /// Ввод кончился — читать больше нечего.
    Closed,
}

/// Клавиши из потока байтов терминала.
struct Input {
    buffer: [u8; 64],
    at: usize,
    len: usize,
}

impl Input {
    const fn new() -> Self {
        Self { buffer: [0; 64], at: 0, len: 0 }
    }

    /// Дочитать порцию. `wait_ms` меньше нуля — ждать сколько угодно.
    fn fill(&mut self, wait_ms: i64) -> bool {
        if wait_ms >= 0 && !stdin_ready(wait_ms) {
            return false;
        }
        let got = read_stdin(&mut self.buffer);
        if got <= 0 {
            return false;
        }
        self.at = 0;
        self.len = got as usize;
        true
    }

    fn byte(&mut self, wait_ms: i64) -> Option<u8> {
        if self.at == self.len && !self.fill(wait_ms) {
            return None;
        }
        let byte = self.buffer[self.at];
        self.at += 1;
        Some(byte)
    }

    /// Есть ли уже набранное — тогда кадр можно не показывать.
    fn pending(&self) -> bool {
        self.at < self.len || stdin_ready(0)
    }

    fn key(&mut self) -> Key {
        let Some(byte) = self.byte(-1) else {
            return Key::Closed;
        };
        match byte {
            0x1b => self.escape(),
            b'\n' | b'\r' => Key::Enter,
            b'\t' => Key::Tab,
            // Backspace приезжает как 0x7F — так его присылает всякий терминал.
            0x7f | 0x08 => Key::Backspace,
            0x01..=0x1a => Key::Ctrl(byte - 1 + b'a'),
            0x20..=0x7e => Key::Char(byte as char),
            0xc0..=0xf7 => self.utf8(byte),
            _ => Key::Unknown,
        }
    }

    /// Буква не из ASCII: русская раскладка присылает UTF-8.
    fn utf8(&mut self, lead: u8) -> Key {
        let need = if lead >= 0xf0 {
            3
        } else if lead >= 0xe0 {
            2
        } else {
            1
        };
        let mut bytes = [lead, 0, 0, 0];
        for slot in bytes.iter_mut().skip(1).take(need) {
            let Some(byte) = self.byte(ESCAPE_WAIT_MS) else {
                return Key::Unknown;
            };
            *slot = byte;
        }
        match core::str::from_utf8(&bytes[..=need]).ok().and_then(|s| s.chars().next()) {
            Some(ch) => Key::Char(ch),
            None => Key::Unknown,
        }
    }

    fn escape(&mut self) -> Key {
        // Одинокий `ESC` — клавиша Escape. Отличить её от начала
        // последовательности можно только временем: хвоста нет за отведённый
        // срок — значит, его и не будет.
        let Some(next) = self.byte(ESCAPE_WAIT_MS) else {
            return Key::Escape;
        };
        match next {
            b'[' => self.csi(),
            // `ESC O P..S` — F1..F4 из VT100.
            b'O' => match self.byte(ESCAPE_WAIT_MS) {
                Some(b'P') => Key::F(1),
                Some(b'Q') => Key::F(2),
                Some(b'R') => Key::F(3),
                Some(b'S') => Key::F(4),
                Some(b'H') => Key::Home,
                Some(b'F') => Key::End,
                _ => Key::Unknown,
            },
            0x1b => Key::Escape,
            _ => Key::Unknown,
        }
    }

    /// Хвост `ESC [ …`: доедается до финального байта, иначе остаток
    /// разобрался бы отдельными клавишами.
    fn csi(&mut self) -> Key {
        let mut first = 0u32;
        let mut separated = false;
        loop {
            let Some(byte) = self.byte(ESCAPE_WAIT_MS) else {
                return Key::Unknown;
            };
            match byte {
                b'0'..=b'9' if !separated => {
                    first = (first * 10 + u32::from(byte - b'0')).min(999);
                }
                b';' => separated = true,
                0x40..=0x7e => {
                    return match byte {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => Key::Right,
                        b'D' => Key::Left,
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        b'~' => match first {
                            1 | 7 => Key::Home,
                            2 => Key::Insert,
                            3 => Key::Delete,
                            4 | 8 => Key::End,
                            5 => Key::PageUp,
                            6 => Key::PageDown,
                            11..=15 => Key::F((first - 10) as u8),
                            17..=21 => Key::F((first - 11) as u8),
                            23 => Key::F(11),
                            24 => Key::F(12),
                            _ => Key::Unknown,
                        },
                        _ => Key::Unknown,
                    };
                }
                _ => {}
            }
        }
    }
}

/// Как часто, ожидая клавишу, сверять размер окна.
///
/// Сообщить программе «окно изменилось» договор не умеет, поэтому `mc`
/// спрашивает сам. Четыре раза в секунду — это системный вызов, который ничего не
/// стоит, а за четверть секунды человек не успевает заметить, что панели
/// отстали от рамки.
const RESIZE_CHECK_MS: i64 = 250;

/// Размер окна терминала в знаках.
fn window_cells() -> (usize, usize) {
    let (cols, rows) = window_size();
    // Нули — окна нет, система в серийной консоли. Рисуем как на терминале
    // 80×24: у серийного терминала на том конце размер обычно такой.
    if cols == 0 || rows == 0 {
        (80, 24)
    } else {
        (cols as usize, rows as usize)
    }
}

/// Готов ли ввод в пределах срока.
fn stdin_ready(wait_ms: i64) -> bool {
    let mut fds = [PollFd { fd: FD_STDIN as i64, wanted: POLL_IN, ..PollFd::default() }];
    poll(&mut fds, wait_ms) > 0
}

// ---------------------------------------------------------------------------
// Пути и слова
// ---------------------------------------------------------------------------

fn join(dir: &str, name: &str) -> String {
    // Корень уже кончается разделителем — второй сделал бы `//`, а это другой
    // путь для всякого, кто сравнивает строки.
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(cut) => String::from(&trimmed[..cut]),
    }
}

fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rfind('/').map_or(trimmed, |cut| &trimmed[cut + 1..])
}

/// Путь, набранный человеком, относительно каталога панели.
fn resolve(dir: &str, typed: &str) -> String {
    if typed.starts_with('/') {
        String::from(typed)
    } else {
        join(dir, typed)
    }
}

fn is_directory(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0 && info.kind == KIND_DIRECTORY
}

fn exists(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0
}

fn error_text(code: i64) -> String {
    match code {
        ERR_NOT_FOUND => String::from("нет такого файла или папки"),
        ERR_PERMISSION => String::from("нет прав"),
        ERR_EXISTS => String::from("такое имя уже есть"),
        ERR_UNSUPPORTED => String::from("этот том не умеет записи"),
        ERR_NOT_EMPTY => String::from("папка не пуста"),
        ERR_NO_SPACE => String::from("нет места"),
        _ => format!("код {code}"),
    }
}

/// Слово в нужном числе: «1 файл», «3 файла», «5 файлов».
fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (n10, n100) = (n % 10, n % 100);
    if (11..=14).contains(&n100) {
        many
    } else if n10 == 1 {
        one
    } else if (2..=4).contains(&n10) {
        few
    } else {
        many
    }
}

/// Число с разрядами через пробел: «1 234 567».
fn grouped(value: u64) -> String {
    let digits = format!("{value}");
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Размер, умещённый в `width` знаков.
fn size_fit(bytes: u64, width: usize) -> String {
    let full = grouped(bytes);
    if full.chars().count() <= width {
        return full;
    }
    let plain = format!("{bytes}");
    if plain.len() <= width {
        return plain;
    }
    let mut value = bytes;
    for unit in ["К", "М", "Г", "Т"] {
        value /= 1024;
        let text = format!("{value}{unit}");
        if text.chars().count() <= width {
            return text;
        }
    }
    String::from("…")
}

/// Путь, умещённый в ширину: обрезается слева, потому что важен конец.
fn path_fit(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width || width < 2 {
        return String::from(path);
    }
    let mut out = String::from("…");
    out.extend(path.chars().skip(count - (width - 1)));
    out
}

/// Разбить текст на строки не длиннее `width` по пробелам.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut len = 0;
        for word in paragraph.split(' ') {
            let word_len = word.chars().count();
            if len > 0 && len + 1 + word_len > width {
                lines.push(core::mem::take(&mut line));
                len = 0;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += word_len;
        }
        lines.push(line);
    }
    lines
}

// ---------------------------------------------------------------------------
// Панель
// ---------------------------------------------------------------------------

struct Entry {
    name: String,
    dir: bool,
    size: u64,
}

struct Panel {
    path: String,
    entries: Vec<Entry>,
    cursor: usize,
    top: usize,
    /// Сколько записей не поместилось в [`MAX_ENTRIES`].
    dropped: usize,
}

impl Panel {
    fn new(path: &str) -> Self {
        Self { path: String::from(path), entries: Vec::new(), cursor: 0, top: 0, dropped: 0 }
    }

    /// Прочитать каталог. `false` — не открылся, и прежний список остаётся.
    fn load(&mut self) -> bool {
        let fd = open(&self.path);
        if fd < 0 {
            return false;
        }
        let mut entries = Vec::new();
        let mut dropped = 0;
        // Первой строкой всегда «..», даже в корне: палец не ищет её каждый раз
        // заново.
        entries.push(Entry { name: String::from(".."), dir: true, size: 0 });
        let mut dirent = Dirent::default();
        while readdir(fd, &mut dirent) {
            let Some(name) = dirent.name() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            if entries.len() >= MAX_ENTRIES || entries.try_reserve(1).is_err() {
                dropped += 1;
                continue;
            }
            entries.push(Entry {
                name: String::from(name),
                dir: dirent.kind == KIND_DIRECTORY,
                size: dirent.size,
            });
        }
        close(fd);
        // Папки над файлами, внутри — по имени без учёта регистра, как в Far.
        // Порядок записей на носителе случаен, а список, который перемешивается
        // от копирования соседнего файла, нельзя читать глазами.
        entries[1..].sort_unstable_by(|a, b| {
            b.dir.cmp(&a.dir).then_with(|| {
                a.name
                    .chars()
                    .flat_map(char::to_lowercase)
                    .cmp(b.name.chars().flat_map(char::to_lowercase))
            })
        });
        self.entries = entries;
        self.dropped = dropped;
        self.cursor = self.cursor.min(self.entries.len() - 1);
        true
    }

    /// Перечитать, оставив курсор на той же записи — или на том же месте, если
    /// записи больше нет.
    fn refresh(&mut self) {
        let keep = self.current().map(|entry| entry.name.clone());
        if !self.load() {
            // Каталог исчез из-под панели (его удалили из другой) — поднимаемся
            // до корня, а не показываем список, которого нет.
            self.path = String::from("/");
            self.cursor = 0;
            self.load();
        }
        if let Some(name) = keep {
            self.select(&name);
        }
    }

    fn select(&mut self, name: &str) {
        if let Some(index) = self.entries.iter().position(|entry| entry.name == name) {
            self.cursor = index;
        }
    }

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn step(&mut self, delta: isize) {
        let last = self.entries.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    /// Окно прокрутки едет за курсором, а не курсор за окном.
    fn keep_visible(&mut self, rows: usize) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
        let max_top = self.entries.len().saturating_sub(rows);
        self.top = self.top.min(max_top);
    }
}

/// Нарисовать панель: рамка, столбцы, список, строка о файле, итог.
fn draw_panel(canvas: &mut Canvas, panel: &Panel, x: usize, w: usize, h: usize, active: bool) {
    let (fg, bg) = (ink::FRAME, ink::PANEL);
    canvas.fill(0, x, w, h, ' ', ink::FILE, bg);
    canvas.double_box(0, x, w, h, fg, bg);
    let inner = w.saturating_sub(2);
    // Столбец размера — когда имени остаётся хотя бы пятнадцать знаков.
    let size_w = if inner >= 24 { 8 } else { 0 };
    let name_w = if size_w > 0 { inner - size_w - 1 } else { inner };
    let sep = x + 1 + name_w;
    let list_rows = h.saturating_sub(5);
    let info_row = h.saturating_sub(2);
    let rule_row = h.saturating_sub(3);

    if size_w > 0 {
        canvas.put(0, sep, '╤', fg, bg);
        for row in 1..rule_row {
            canvas.put(row, sep, '│', fg, bg);
        }
        canvas.centered(1, sep + 1, size_w, "Размер", ink::TITLE, bg);
    }
    canvas.centered(1, x + 1, name_w, "Имя", ink::TITLE, bg);

    for i in 0..list_rows {
        let row = 2 + i;
        let index = panel.top + i;
        let Some(entry) = panel.entries.get(index) else {
            break;
        };
        let selected = active && index == panel.cursor;
        let (ink_fg, ink_bg) = if selected {
            (ink::CURSOR_FG, ink::CURSOR_BG)
        } else if entry.dir {
            (ink::DIR, bg)
        } else {
            (ink::FILE, bg)
        };
        canvas.text(row, x + 1, name_w, &entry.name, ink_fg, ink_bg);
        if size_w > 0 {
            canvas.put(row, sep, '│', if selected { ink::CURSOR_FG } else { fg }, ink_bg);
            let size = size_label(entry, size_w);
            canvas.right(row, sep + 1, size_w, &size, ink_fg, ink_bg);
            // Промежуток слева от размера — тем же цветом, что и курсор.
            let used = size.chars().count().min(size_w);
            canvas.fill(row, sep + 1, size_w - used, 1, ' ', ink_fg, ink_bg);
        }
    }

    // Полоса прокрутки на правой рамке — только когда список не помещается.
    let total = panel.entries.len();
    if total > list_rows && list_rows >= 2 {
        let span = total - list_rows;
        let thumb = panel.top * (list_rows - 1) / span.max(1);
        for i in 0..list_rows {
            let ch = if i == thumb { '█' } else { '░' };
            canvas.put(2 + i, x + w - 1, ch, fg, bg);
        }
    }

    // Разделитель и строка о файле под курсором.
    canvas.put(rule_row, x, '╟', fg, bg);
    canvas.fill(rule_row, x + 1, inner, 1, '─', fg, bg);
    canvas.put(rule_row, x + w - 1, '╢', fg, bg);
    if size_w > 0 {
        canvas.put(rule_row, sep, '┴', fg, bg);
    }
    if let Some(entry) = panel.current() {
        let size = size_label(entry, 12);
        let size_len = size.chars().count();
        let room = inner.saturating_sub(size_len + 1);
        canvas.text(info_row, x + 1, room, &entry.name, ink::FILE, bg);
        canvas.right(info_row, x + 1, inner, &size, ink::FILE, bg);
    }

    // Путь в заголовке: у активной панели — цветом курсора, как в Far.
    let title = format!(" {} ", path_fit(&panel.path, w.saturating_sub(6)));
    let (title_fg, title_bg) = if active { (ink::CURSOR_FG, ink::CURSOR_BG) } else { (ink::FILE, bg) };
    canvas.centered(0, x + 1, inner, &title, title_fg, title_bg);

    // Итог на нижней рамке.
    let files = panel.entries.iter().filter(|e| !e.dir).count();
    let dirs = panel.entries.iter().filter(|e| e.dir && e.name != "..").count();
    let bytes: u64 = panel.entries.iter().filter(|e| !e.dir).map(|e| e.size).sum();
    let summary = if panel.dropped > 0 {
        format!(" список обрезан: не показано {} ", panel.dropped)
    } else {
        format!(
            " {} {}, {} {}, {} Б ",
            dirs,
            plural(dirs, "папка", "папки", "папок"),
            files,
            plural(files, "файл", "файла", "файлов"),
            grouped(bytes)
        )
    };
    let summary_ink = if panel.dropped > 0 { ink::TITLE } else { ink::FILE };
    canvas.centered(h.saturating_sub(1), x + 1, inner, &summary, summary_ink, bg);
}

fn size_label(entry: &Entry, width: usize) -> String {
    if entry.name == ".." {
        String::from("Вверх")
    } else if entry.dir {
        String::from("Папка")
    } else {
        size_fit(entry.size, width)
    }
}

// ---------------------------------------------------------------------------
// Диалоги
// ---------------------------------------------------------------------------

/// Поле ввода внутри диалога.
struct Field {
    text: Vec<char>,
    cursor: usize,
    /// Текст «выделен»: первая набранная буква заменяет его целиком, как в
    /// поле с выделенным именем в Windows и Far.
    picked: bool,
}

impl Field {
    fn new(text: &str, picked: bool) -> Self {
        let text: Vec<char> = text.chars().take(FIELD_MAX).collect();
        let cursor = text.len();
        let picked = picked && !text.is_empty();
        Self { text, cursor, picked }
    }

    fn value(&self) -> String {
        self.text.iter().collect()
    }

    fn handle(&mut self, key: Key) {
        match key {
            Key::Char(ch) => {
                if self.picked {
                    self.text.clear();
                    self.cursor = 0;
                }
                if self.text.len() < FIELD_MAX {
                    self.text.insert(self.cursor, ch);
                    self.cursor += 1;
                }
            }
            Key::Backspace | Key::Delete if self.picked => {
                self.text.clear();
                self.cursor = 0;
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
            }
            Key::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.text.len(),
            _ => return,
        }
        self.picked = false;
    }
}

struct Dialog<'a> {
    title: &'a str,
    text: Vec<String>,
    field: Option<Field>,
    buttons: &'a [&'a str],
    /// Красное окно: удаление и ошибки.
    danger: bool,
}

// ---------------------------------------------------------------------------
// Программа
// ---------------------------------------------------------------------------

struct App {
    canvas: Canvas,
    input: Input,
    left: Panel,
    right: Panel,
    left_active: bool,
    message: Option<String>,
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };

    let mut left = Panel::new(args.get(1).unwrap_or("/"));
    let mut right = Panel::new(args.get(2).unwrap_or("/"));
    if !left.load() {
        error("mc: cannot open the left directory\n");
        exit(1);
    }
    if !right.load() {
        // Правая панель не критична: система, где есть только один читаемый
        // каталог, — это всё ещё система, в которой можно работать.
        right.path = String::from("/");
        right.load();
    }

    let mut app = App {
        canvas: Canvas::new(),
        input: Input::new(),
        left,
        right,
        left_active: true,
        message: None,
    };
    app.canvas.fit_window();

    // Прямой режим: клавиши приезжают немедленно и без эха. Без него `mc`
    // получал бы строки по Enter, а стрелки — четырьмя видимыми символами.
    set_raw(true);
    error("mc: started\n");

    app.run();

    // Экран возвращается оболочке очищенным, с курсором в углу и без нашего
    // цвета. Программа, ушедшая, не прибрав за собой, ломает приглашение того,
    // кто её запустил.
    set_raw(false);
    print("\x1b[0m\x1b[2J\x1b[1;1H\x1b[?25l");
    error("mc: quit\n");
    println("mc: done");
    exit(0)
}

impl App {
    fn active(&self) -> &Panel {
        if self.left_active { &self.left } else { &self.right }
    }

    fn active_mut(&mut self) -> &mut Panel {
        if self.left_active { &mut self.left } else { &mut self.right }
    }

    fn passive(&self) -> &Panel {
        if self.left_active { &self.right } else { &self.left }
    }

    fn list_rows(&self) -> usize {
        self.canvas.rows.saturating_sub(7).max(1)
    }

    /// Запись под курсором, кроме «..»: каталог, имя, папка ли.
    fn selected(&self) -> Option<(String, String, bool)> {
        let panel = self.active();
        let entry = panel.current()?;
        if entry.name == ".." {
            return None;
        }
        Some((panel.path.clone(), entry.name.clone(), entry.dir))
    }

    /// Следующая клавиша — или [`Key::Resize`], если, пока её ждали, окно
    /// изменило размер.
    fn next_key(&mut self) -> Key {
        loop {
            if self.input.at < self.input.len || stdin_ready(RESIZE_CHECK_MS) {
                return self.input.key();
            }
            if window_cells() != (self.canvas.cols, self.canvas.rows) {
                return Key::Resize;
            }
        }
    }

    fn run(&mut self) {
        loop {
            self.canvas.fit_window();
            self.draw();
            if !self.input.pending() {
                self.canvas.present();
            }
            let key = self.next_key();
            if !matches!(key, Key::Unknown | Key::Resize) {
                self.message = None;
            }
            let page = self.list_rows() as isize;
            match key {
                Key::Closed | Key::F(10) => return,
                Key::Tab => self.left_active = !self.left_active,
                Key::Up => self.active_mut().step(-1),
                Key::Down => self.active_mut().step(1),
                Key::PageUp => self.active_mut().step(-page),
                Key::PageDown => self.active_mut().step(page),
                Key::Home => self.active_mut().cursor = 0,
                Key::End => {
                    let panel = self.active_mut();
                    panel.cursor = panel.entries.len().saturating_sub(1);
                }
                Key::Enter => self.enter(),
                Key::F(1) => self.help(),
                Key::F(3) => {
                    if let Some((dir, name, false)) = self.selected() {
                        self.view(join(&dir, &name));
                    }
                }
                Key::F(4) => {
                    if let Some((dir, name, false)) = self.selected() {
                        self.edit(join(&dir, &name));
                    }
                }
                Key::F(5) => self.copy(),
                Key::F(6) => self.rename(),
                Key::F(7) => self.make_dir(),
                Key::F(8) => self.delete(),
                Key::Ctrl(b'r') => {
                    self.left.refresh();
                    self.right.refresh();
                    self.message = Some(String::from("Панели перечитаны"));
                }
                // Перерисовать экран целиком, как Ctrl+L в mc. Нужно не для
                // красоты: ядро печатает в окно оболочки и своё («#12 /bin/sshd:
                // exited…»), и строка поверх рамки иначе висела бы, пока эту
                // строку экрана не изменит сам `mc`.
                Key::Ctrl(b'l') => self.canvas.fresh = true,
                _ => {}
            }
        }
    }

    fn draw(&mut self) {
        let (cols, rows) = (self.canvas.cols, self.canvas.rows);
        self.canvas.cursor = None;
        self.canvas.fill(0, 0, cols, rows, ' ', ink::LINE_FG, ink::LINE_BG);
        if cols < MIN_COLS || rows < MIN_ROWS {
            self.canvas.text(0, 0, cols, "Окно мало для панелей (нужно 40×10). F10 — выход.", ink::LINE_FG, ink::LINE_BG);
            return;
        }
        let list = self.list_rows();
        self.left.keep_visible(list);
        self.right.keep_visible(list);
        let half = cols / 2;
        let height = rows - 2;
        draw_panel(&mut self.canvas, &self.left, 0, half, height, self.left_active);
        draw_panel(&mut self.canvas, &self.right, half, cols - half, height, !self.left_active);
        let line = format!(" {}", self.message.as_deref().unwrap_or(HINT));
        self.canvas.text(rows - 2, 0, cols, &line, ink::LINE_FG, ink::LINE_BG);
        keybar(&mut self.canvas, rows - 1, &PANEL_KEYS);
    }

    /// Войти в каталог под курсором, подняться выше или открыть файл.
    fn enter(&mut self) {
        let panel = self.active();
        let Some(entry) = panel.current() else {
            return;
        };
        let (target, came_from) = if entry.name == ".." {
            (parent(&panel.path), Some(String::from(base_name(&panel.path))))
        } else if entry.dir {
            (join(&panel.path, &entry.name), None)
        } else {
            // Файл открывается просмотром: Enter — «открыть», а запускать чужой
            // код клавишей, которой ходят по каталогам, никто не ждёт.
            let path = join(&panel.path, &entry.name);
            self.view(path);
            return;
        };

        let panel = self.active_mut();
        let previous = core::mem::replace(&mut panel.path, target);
        let old_cursor = panel.cursor;
        panel.cursor = 0;
        if !panel.load() {
            // Каталог не открылся: возвращаемся туда, где были, а не остаёмся с
            // пустой панелью и путём, которого нет.
            panel.path = previous;
            panel.cursor = old_cursor;
            panel.load();
            error("mc: cannot enter the directory\n");
            self.message = Some(String::from("Не удалось открыть папку"));
            return;
        }
        panel.top = 0;
        // Поднявшись на уровень, курсор встаёт на папку, из которой вышли, —
        // так в Far, и так проще вернуться обратно.
        if let Some(name) = came_from {
            panel.select(&name);
        }
        let path = panel.path.clone();
        error(&format!("mc: entered {path}\n"));
    }

    /// Показать диалог поверх того, что на холсте. Возвращает номер нажатой
    /// кнопки и текст поля; `None` — отменён.
    fn dialog(&mut self, mut dialog: Dialog<'_>) -> Option<(usize, String)> {
        let mut backdrop = self.canvas.cells.clone();
        let has_field = dialog.field.is_some();
        let stops = dialog.buttons.len() + usize::from(has_field);
        let mut focus = 0;
        loop {
            self.canvas.fit_window();
            if self.canvas.cells.len() != backdrop.len() {
                // Окно изменилось под открытым вопросом: прежний фон другого
                // размера, и под диалогом рисуются панели заново. Для вопроса из
                // правки это панели, а не текст файла, — названное упрощение:
                // иначе каждому месту вызова пришлось бы отдавать сюда своё
                // рисование.
                self.draw();
                backdrop = self.canvas.cells.clone();
            }
            self.canvas.cells.copy_from_slice(&backdrop);
            self.canvas.cursor = None;
            draw_dialog(&mut self.canvas, &dialog, focus);
            if !self.input.pending() {
                self.canvas.present();
            }
            let key = self.next_key();
            let on_field = has_field && focus == 0;
            match key {
                Key::Closed | Key::Escape | Key::F(10) => return None,
                Key::Enter => {
                    let button = if on_field { 0 } else { focus - usize::from(has_field) };
                    let text = dialog.field.as_ref().map(Field::value).unwrap_or_default();
                    return Some((button, text));
                }
                Key::Tab => focus = (focus + 1) % stops,
                Key::Left | Key::Up if !on_field => {
                    focus = focus.saturating_sub(1).max(usize::from(has_field && focus > 0));
                }
                Key::Right | Key::Down if !on_field => focus = (focus + 1).min(stops - 1),
                Key::Down if on_field => focus = (focus + 1).min(stops - 1),
                _ if on_field => {
                    if let Some(field) = dialog.field.as_mut() {
                        field.handle(key);
                    }
                }
                _ => {}
            }
        }
    }

    fn fail(&mut self, title: &str, text: String) {
        self.dialog(Dialog { title, text: vec![text], field: None, buttons: &["OK"], danger: true });
    }

    fn help(&mut self) {
        let text = vec![
            String::from("Tab — другая панель, Enter — войти в папку или просмотреть файл"),
            String::from("F3 — просмотр, F4 — правка, F5 — копия в другую панель"),
            String::from("F6 — переименовать или перенести, F7 — новая папка, F8 — удалить"),
            String::from("Ctrl+R — перечитать панели, F10 — выход"),
            String::from("В просмотре F4 переключает текст и байты, в правке F2 сохраняет."),
            String::from("Выделения нескольких файлов и копирования папок пока нет — функции запланированы."),
        ];
        self.dialog(Dialog { title: "Помощь", text, field: None, buttons: &["Закрыть"], danger: false });
    }

    fn copy(&mut self) {
        let Some((dir, name, is_dir)) = self.selected() else {
            return;
        };
        if is_dir {
            error("mc: only files are copied\n");
            self.fail("Копирование", String::from("Папки пока не копируются — функция запланирована."));
            return;
        }
        let target_dir = self.passive().path.clone();
        let Some((0, typed)) = self.dialog(Dialog {
            title: "Копирование",
            text: vec![format!("Копировать «{name}» в:")],
            field: Some(Field::new(&target_dir, true)),
            buttons: &["Копировать", "Отмена"],
            danger: false,
        }) else {
            return;
        };
        let typed = typed.trim();
        if typed.is_empty() {
            return;
        }
        let from = join(&dir, &name);
        let target = resolve(&dir, typed);
        let to = if is_directory(&target) { join(&target, &name) } else { target };
        if to == from {
            self.fail("Копирование", String::from("Файл нельзя скопировать сам в себя."));
            return;
        }
        if exists(&to) {
            let answer = self.dialog(Dialog {
                title: "Копирование",
                text: vec![format!("«{to}» уже существует. Заменить?")],
                field: None,
                buttons: &["Заменить", "Отмена"],
                danger: true,
            });
            if !matches!(answer, Some((0, _))) {
                return;
            }
        }
        match copy_file(&from, &to) {
            Ok(bytes) => {
                // Строка, ради которой всё это проверяется снаружи: она называет
                // обе стороны, поэтому «скопировалось не то» и «скопировалось»
                // выглядят по-разному.
                error(&format!("mc: copied {from} -> {to}\n"));
                self.message = Some(format!("Скопировано: {name} → {to}, {} Б", grouped(bytes)));
                self.left.refresh();
                self.right.refresh();
            }
            Err(text) => self.fail("Копирование", text),
        }
    }

    /// Переименовать или перенести.
    ///
    /// Именно переименовать, а не «скопировать и удалить»: содержимое не
    /// читается вовсе, меняется запись каталога.
    fn rename(&mut self) {
        let Some((dir, name, _)) = self.selected() else {
            return;
        };
        let Some((0, typed)) = self.dialog(Dialog {
            title: "Переименование",
            text: vec![format!("Переименовать или перенести «{name}» в:")],
            field: Some(Field::new(&name, true)),
            buttons: &["Переименовать", "Отмена"],
            danger: false,
        }) else {
            return;
        };
        let typed = typed.trim();
        if typed.is_empty() || typed == name {
            return;
        }
        let from = join(&dir, &name);
        let target = resolve(&dir, typed);
        let to = if target != from && is_directory(&target) { join(&target, &name) } else { target };
        let result = rename(&from, &to);
        if result < 0 {
            error(&format!("mc: rename {from}: {result}\n"));
            self.fail("Переименование", format!("Не удалось: {}", error_text(result)));
            return;
        }
        error(&format!("mc: renamed {from} -> {to}\n"));
        self.message = Some(format!("Переименовано: {name} → {to}"));
        self.left.refresh();
        self.right.refresh();
        // Курсор остаётся на переименованном — его и искать глазами.
        if parent(&to) == self.active().path {
            let new_name = String::from(base_name(&to));
            self.active_mut().select(&new_name);
        }
    }

    fn make_dir(&mut self) {
        let dir = self.active().path.clone();
        let Some((0, typed)) = self.dialog(Dialog {
            title: "Создание папки",
            text: vec![String::from("Имя новой папки:")],
            field: Some(Field::new("", false)),
            buttons: &["Создать", "Отмена"],
            danger: false,
        }) else {
            return;
        };
        let typed = typed.trim();
        if typed.is_empty() {
            return;
        }
        let path = resolve(&dir, typed);
        let result = mkdir(&path, 0o755);
        if result < 0 {
            error(&format!("mc: mkdir {path}: {result}\n"));
            self.fail("Создание папки", format!("Не удалось: {}", error_text(result)));
            return;
        }
        error(&format!("mc: created {path}\n"));
        self.message = Some(format!("Создана папка {path}"));
        self.left.refresh();
        self.right.refresh();
        if parent(&path) == self.active().path {
            let name = String::from(base_name(&path));
            self.active_mut().select(&name);
        }
    }

    fn delete(&mut self) {
        let Some((dir, name, is_dir)) = self.selected() else {
            return;
        };
        let what = if is_dir { "папку" } else { "файл" };
        let answer = self.dialog(Dialog {
            title: "Удаление",
            text: vec![format!("Удалить {what} «{name}»?")],
            field: None,
            buttons: &["Удалить", "Отмена"],
            danger: true,
        });
        if !matches!(answer, Some((0, _))) {
            return;
        }
        let path = join(&dir, &name);
        let result = remove(&path);
        if result < 0 {
            error(&format!("mc: remove {path}: {result}\n"));
            self.fail("Удаление", format!("Не удалось удалить «{name}»: {}", error_text(result)));
            return;
        }
        error(&format!("mc: removed {path}\n"));
        self.message = Some(format!("Удалено: {name}"));
        // Курсор остаётся на том же месте списка, а не прыгает к «..»: следующий
        // файл встаёт под него сам.
        let index = self.active().cursor;
        self.left.refresh();
        self.right.refresh();
        let panel = self.active_mut();
        panel.cursor = index.min(panel.entries.len().saturating_sub(1));
    }

    // -----------------------------------------------------------------------
    // Просмотр (F3)
    // -----------------------------------------------------------------------

    fn view(&mut self, path: String) {
        let mut viewer = match Viewer::open(&path) {
            Ok(viewer) => viewer,
            Err(text) => {
                error(&format!("mc: cannot view {path}\n"));
                self.fail("Просмотр", text);
                return;
            }
        };
        error(&format!("mc: viewed {path}\n"));
        loop {
            self.canvas.fit_window();
            viewer.draw(&mut self.canvas);
            if !self.input.pending() {
                self.canvas.present();
            }
            let rows = viewer.text_rows(&self.canvas);
            match self.next_key() {
                Key::Closed | Key::Escape | Key::F(3) | Key::F(10) | Key::Char('q') => break,
                Key::Ctrl(b'l') => self.canvas.fresh = true,
                Key::F(4) => {
                    viewer.hex = !viewer.hex;
                    viewer.top = 0;
                    viewer.left = 0;
                }
                Key::Up => viewer.top = viewer.top.saturating_sub(1),
                Key::Down => viewer.top += 1,
                Key::PageUp => viewer.top = viewer.top.saturating_sub(rows),
                Key::PageDown | Key::Char(' ') => viewer.top += rows,
                Key::Home => {
                    viewer.top = 0;
                    viewer.left = 0;
                }
                Key::End => viewer.top = usize::MAX,
                Key::Left => viewer.left = viewer.left.saturating_sub(8),
                Key::Right if !viewer.hex => viewer.left += 8,
                _ => {}
            }
            viewer.clamp(rows);
        }
        let rows = viewer.text_rows(&self.canvas);
        let (first, last, total) = viewer.span(rows);
        error(&format!("mc: viewer closed showing lines {first}-{last} of {total}\n"));
    }

    // -----------------------------------------------------------------------
    // Правка (F4)
    // -----------------------------------------------------------------------

    fn edit(&mut self, path: String) {
        let mut editor = match Editor::open(&path) {
            Ok(editor) => editor,
            Err(text) => {
                error(&format!("mc: cannot edit {path}\n"));
                self.fail("Правка", text);
                return;
            }
        };
        error(&format!("mc: editing {path}, {} line(s)\n", editor.lines.len()));
        loop {
            self.canvas.fit_window();
            let rows = self.canvas.rows.saturating_sub(2).max(1);
            editor.scroll(rows, self.canvas.cols.max(1));
            editor.draw(&mut self.canvas);
            if !self.input.pending() {
                self.canvas.present();
            }
            let key = self.next_key();
            if key != Key::Resize {
                editor.note = None;
            }
            match key {
                Key::Closed => break,
                Key::F(2) | Key::Ctrl(b's') => {
                    self.save(&mut editor);
                }
                Key::Ctrl(b'l') => self.canvas.fresh = true,
                Key::F(10) | Key::Escape => {
                    if !editor.modified {
                        break;
                    }
                    let answer = self.dialog(Dialog {
                        title: "Правка",
                        text: vec![format!("Файл «{}» изменён. Сохранить?", base_name(&editor.path))],
                        field: None,
                        buttons: &["Сохранить", "Не сохранять", "Продолжить"],
                        danger: false,
                    });
                    match answer {
                        Some((0, _)) => {
                            if self.save(&mut editor) {
                                break;
                            }
                        }
                        Some((1, _)) => break,
                        _ => {}
                    }
                }
                other => editor.handle(other, rows),
            }
        }
        error("mc: editor closed\n");
        self.left.refresh();
        self.right.refresh();
    }

    fn save(&mut self, editor: &mut Editor) -> bool {
        match editor.save() {
            Ok(bytes) => {
                error(&format!("mc: saved {}, {bytes} bytes\n", editor.path));
                editor.note = Some(format!("Сохранено: {} Б", grouped(bytes as u64)));
                true
            }
            Err(text) => {
                error(&format!("mc: cannot save {}\n", editor.path));
                self.fail("Сохранение", text);
                false
            }
        }
    }
}

/// Нарисовать диалог: серое окно с двойной рамкой, тенью и кнопками.
fn draw_dialog(canvas: &mut Canvas, dialog: &Dialog<'_>, focus: usize) {
    let (cols, rows) = (canvas.cols, canvas.rows);
    let limit = cols.saturating_sub(12).max(10);
    let mut lines = Vec::new();
    for text in &dialog.text {
        lines.extend(wrap(text, limit));
    }
    let buttons_len: usize =
        dialog.buttons.iter().map(|b| b.chars().count() + 4).sum::<usize>() + 2 * dialog.buttons.len();
    let mut content = dialog.title.chars().count() + 6;
    content = content.max(buttons_len);
    for line in &lines {
        content = content.max(line.chars().count());
    }
    if dialog.field.is_some() {
        content = content.max(40);
    }
    let content = content.min(limit);
    let has_field = dialog.field.is_some();
    let w = content + 8;
    let h = lines.len() + usize::from(has_field) + 6;
    let x = cols.saturating_sub(w) / 2;
    let y = rows.saturating_sub(h) / 2;
    let (fg, bg) = if dialog.danger {
        (ink::DANGER_FG, ink::DANGER_BG)
    } else {
        (ink::DIALOG_FG, ink::DIALOG_BG)
    };

    canvas.fill(y, x, w, h, ' ', fg, bg);
    canvas.shade(y + 1, x + w, 2, h);
    canvas.shade(y + h, x + 2, w, 1);
    canvas.double_box(y + 1, x + 2, w - 4, h - 2, fg, bg);
    let title = format!(" {} ", dialog.title);
    canvas.centered(y + 1, x + 3, w - 6, &title, fg, bg);

    let mut row = y + 2;
    for line in &lines {
        canvas.text(row, x + 4, content, line, fg, bg);
        row += 1;
    }
    if let Some(field) = &dialog.field {
        let (field_fg, field_bg) = if field.picked {
            (ink::PICKED_FG, ink::PICKED_BG)
        } else {
            (ink::FIELD_FG, ink::FIELD_BG)
        };
        let offset = field.cursor.saturating_sub(content.saturating_sub(1));
        let shown: String = field.text.iter().skip(offset).take(content).collect();
        canvas.fill(row, x + 4, content, 1, ' ', ink::FIELD_FG, ink::FIELD_BG);
        let shown_len = shown.chars().count();
        canvas.text(row, x + 4, shown_len, &shown, field_fg, field_bg);
        if focus == 0 {
            canvas.cursor = Some((row, x + 4 + field.cursor - offset));
        }
    }

    let rule = y + h - 4;
    canvas.put(rule, x + 2, '╟', fg, bg);
    canvas.fill(rule, x + 3, w - 6, 1, '─', fg, bg);
    canvas.put(rule, x + w - 3, '╢', fg, bg);

    let mut bx = x + w.saturating_sub(buttons_len) / 2 + 1;
    for (index, label) in dialog.buttons.iter().enumerate() {
        let text = format!("[ {label} ]");
        let focused = focus == index + usize::from(has_field);
        let (b_fg, b_bg) = if focused { (ink::CURSOR_FG, ink::CURSOR_BG) } else { (fg, bg) };
        let len = text.chars().count();
        canvas.text(y + h - 3, bx, len, &text, b_fg, b_bg);
        bx += len + 2;
    }
}

/// Скопировать файл кусками. Возвращает число байт.
fn copy_file(from: &str, to: &str) -> Result<u64, String> {
    let input = open(from);
    if input < 0 {
        error("mc: cannot read the source\n");
        return Err(format!("Не удалось открыть «{from}»: {}", error_text(input)));
    }
    let output = open_write(to, true, true);
    if output < 0 {
        close(input);
        error(&format!("mc: cannot create {to}: {output}\n"));
        return Err(format!("Не удалось создать «{to}»: {}", error_text(output)));
    }
    let mut chunk = [0u8; CHUNK];
    let mut copied = 0u64;
    let mut failure = None;
    loop {
        let got = read(input, &mut chunk);
        if got < 0 {
            failure = Some(got);
            break;
        }
        if got == 0 {
            break;
        }
        let written = write(output, &chunk[..got as usize]);
        if written != got {
            failure = Some(if written < 0 { written } else { ERR_NO_SPACE });
            break;
        }
        copied += written as u64;
    }
    close(input);
    close(output);
    if let Some(code) = failure {
        error("mc: copy failed\n");
        return Err(format!("Копирование прервано: {}", error_text(code)));
    }
    Ok(copied)
}

/// Вывести строку байтов в ряд холста: табуляция до кратного восьми, всё
/// непечатное и не-UTF-8 — точкой.
fn draw_bytes(canvas: &mut Canvas, row: usize, bytes: &[u8], left: usize, fg: u8, bg: u8) {
    let width = canvas.cols;
    let mut column = 0usize;
    let emit = |canvas: &mut Canvas, column: &mut usize, ch: char| {
        if *column >= left && *column - left < width {
            canvas.put(row, *column - left, ch, fg, bg);
        }
        *column += 1;
    };
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if ch == '\t' {
                let stop = (column / 8 + 1) * 8;
                while column < stop {
                    emit(canvas, &mut column, ' ');
                }
            } else if ch.is_control() {
                emit(canvas, &mut column, '.');
            } else {
                emit(canvas, &mut column, ch);
            }
        }
        for _ in chunk.invalid() {
            emit(canvas, &mut column, '.');
        }
    }
}

/// Столбец на экране для позиции в строке — с учётом табуляции.
fn visual_column(line: &str, chars: usize) -> usize {
    let mut column = 0;
    for ch in line.chars().take(chars) {
        column = if ch == '\t' { (column / 8 + 1) * 8 } else { column + 1 };
    }
    column
}

struct Viewer {
    path: String,
    data: Vec<u8>,
    /// Начала строк текста.
    starts: Vec<u32>,
    size: u64,
    top: usize,
    left: usize,
    hex: bool,
    /// Байтов в строке в режиме байтов — зависит от ширины окна.
    per_row: usize,
    /// Показано не всё: файл больше [`VIEW_MAX`] или строк больше
    /// [`VIEW_LINES_MAX`].
    cut: bool,
}

impl Viewer {
    fn open(path: &str) -> Result<Self, String> {
        let fd = open(path);
        if fd < 0 {
            return Err(format!("Не удалось открыть «{path}»: {}", error_text(fd)));
        }
        let size = file_size(fd).max(0) as u64;
        let take = (size as usize).min(VIEW_MAX);
        let mut data = Vec::new();
        if data.try_reserve_exact(take).is_err() {
            close(fd);
            return Err(String::from("Не хватило памяти, чтобы открыть файл."));
        }
        let mut chunk = [0u8; CHUNK];
        while data.len() < take {
            let want = (take - data.len()).min(CHUNK);
            let got = read(fd, &mut chunk[..want]);
            if got <= 0 {
                break;
            }
            data.extend_from_slice(&chunk[..got as usize]);
        }
        close(fd);

        let mut cut = size as usize > data.len();
        // Двоичный файл — это нулевой байт в начале: текст его не содержит, а
        // строками двоичный файл читать бессмысленно.
        let hex = data.iter().take(4096).any(|&b| b == 0);
        let mut starts = Vec::new();
        if starts.try_reserve(64).is_err() {
            return Err(String::from("Не хватило памяти, чтобы разметить строки."));
        }
        starts.push(0u32);
        let mut end = data.len();
        for (index, &byte) in data.iter().enumerate() {
            if byte != b'\n' || index + 1 >= data.len() {
                continue;
            }
            if starts.len() >= VIEW_LINES_MAX || starts.try_reserve(1).is_err() {
                cut = true;
                end = index + 1;
                break;
            }
            starts.push((index + 1) as u32);
        }
        data.truncate(end);
        Ok(Self {
            path: String::from(path),
            data,
            starts,
            size,
            top: 0,
            left: 0,
            hex,
            per_row: 16,
            cut,
        })
    }

    fn total(&self) -> usize {
        if self.hex {
            self.data.len().div_ceil(self.per_row)
        } else if self.data.is_empty() {
            0
        } else {
            self.starts.len()
        }
    }

    fn text_rows(&self, canvas: &Canvas) -> usize {
        canvas.rows.saturating_sub(2).max(1)
    }

    fn clamp(&mut self, rows: usize) {
        self.top = self.top.min(self.total().saturating_sub(rows));
    }

    /// Первая и последняя видимые строки (с единицы) и сколько их всего.
    fn span(&self, rows: usize) -> (usize, usize, usize) {
        let total = self.total();
        if total == 0 {
            return (0, 0, 0);
        }
        (self.top + 1, (self.top + rows).min(total), total)
    }

    fn line(&self, index: usize) -> &[u8] {
        let start = self.starts[index] as usize;
        let end = self.starts.get(index + 1).map_or(self.data.len(), |next| *next as usize);
        let mut line = &self.data[start..end.max(start)];
        if let [rest @ .., b'\n'] = line {
            line = rest;
        }
        if let [rest @ .., b'\r'] = line {
            line = rest;
        }
        line
    }

    fn draw(&mut self, canvas: &mut Canvas) {
        let (cols, rows) = (canvas.cols, canvas.rows);
        self.per_row = if cols >= 78 {
            16
        } else if cols >= 42 {
            8
        } else {
            4
        };
        let text_rows = self.text_rows(canvas);
        self.clamp(text_rows);
        canvas.cursor = None;
        canvas.fill(0, 0, cols, rows, ' ', ink::FILE, ink::PANEL);

        let (first, last, total) = self.span(text_rows);
        let percent = if total == 0 { 100 } else { last * 100 / total };
        let unit = if self.hex { "байты" } else { "строки" };
        let mut status = format!("{unit} {first}–{last} из {total} · {percent}% ");
        if self.cut {
            status = format!("показано не всё ({} из {} Б) · {status}", grouped(self.data.len() as u64), grouped(self.size));
        }
        let status_len = status.chars().count().min(cols);
        canvas.fill(0, 0, cols, 1, ' ', ink::CURSOR_FG, ink::CURSOR_BG);
        canvas.text(0, 1, cols.saturating_sub(status_len + 2), &self.path, ink::CURSOR_FG, ink::CURSOR_BG);
        canvas.right(0, 0, cols, &status, ink::CURSOR_FG, ink::CURSOR_BG);

        for i in 0..text_rows {
            let index = self.top + i;
            if index >= total {
                break;
            }
            let row = 1 + i;
            if self.hex {
                let offset = index * self.per_row;
                let chunk = &self.data[offset..(offset + self.per_row).min(self.data.len())];
                let mut text = String::new();
                let _ = write!(text, "{offset:08x}  ");
                for k in 0..self.per_row {
                    match chunk.get(k) {
                        Some(byte) => {
                            let _ = write!(text, "{byte:02x} ");
                        }
                        None => text.push_str("   "),
                    }
                }
                text.push(' ');
                for byte in chunk {
                    text.push(if (0x20..0x7f).contains(byte) { *byte as char } else { '.' });
                }
                canvas.text(row, 0, cols, &text, ink::FILE, ink::PANEL);
            } else {
                let line = self.line(index);
                draw_bytes(canvas, row, line, self.left, ink::FILE, ink::PANEL);
            }
        }
        keybar(canvas, rows.saturating_sub(1), if self.hex { &VIEW_HEX_KEYS } else { &VIEW_KEYS });
    }
}

struct Editor {
    path: String,
    lines: Vec<String>,
    row: usize,
    /// Позиция в строке — в знаках, а не в байтах.
    col: usize,
    top: usize,
    left: usize,
    modified: bool,
    /// Кончался ли файл переводом строки — сохранение повторяет, как было.
    newline_at_end: bool,
    note: Option<String>,
}

impl Editor {
    fn open(path: &str) -> Result<Self, String> {
        let fd = open(path);
        if fd < 0 {
            return Err(format!("Не удалось открыть «{path}»: {}", error_text(fd)));
        }
        let size = file_size(fd).max(0) as usize;
        if size > EDIT_MAX {
            close(fd);
            return Err(format!(
                "Файл больше {} КиБ — правка таких пока не поддерживается. Просмотр — F3.",
                EDIT_MAX / 1024
            ));
        }
        let mut data = Vec::new();
        if data.try_reserve_exact(size).is_err() {
            close(fd);
            return Err(String::from("Не хватило памяти, чтобы открыть файл."));
        }
        let mut chunk = [0u8; CHUNK];
        loop {
            let got = read(fd, &mut chunk);
            if got <= 0 || data.len() + got as usize > EDIT_MAX {
                break;
            }
            data.extend_from_slice(&chunk[..got as usize]);
        }
        close(fd);
        if data.contains(&0) {
            return Err(String::from("Двоичный файл — правка не поддерживается. Просмотр — F3."));
        }
        let Ok(text) = core::str::from_utf8(&data) else {
            return Err(String::from("Файл не в UTF-8 — правка не поддерживается. Просмотр — F3."));
        };
        let newline_at_end = text.is_empty() || text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines = Vec::new();
        for line in body.split('\n') {
            if lines.try_reserve(1).is_err() {
                return Err(String::from("Не хватило памяти, чтобы открыть файл."));
            }
            lines.push(String::from(line));
        }
        Ok(Self {
            path: String::from(path),
            lines,
            row: 0,
            col: 0,
            top: 0,
            left: 0,
            modified: false,
            newline_at_end,
            note: None,
        })
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Байтовое смещение знака `col` в строке.
    fn byte_at(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices().nth(col).map_or(line.len(), |(at, _)| at)
    }

    fn handle(&mut self, key: Key, rows: usize) {
        match key {
            Key::Char(ch) => self.insert(ch),
            Key::Tab => self.insert('\t'),
            Key::Enter => {
                let at = self.byte_at(self.row, self.col);
                let tail = self.lines[self.row].split_off(at);
                self.lines.insert(self.row + 1, tail);
                self.row += 1;
                self.col = 0;
                self.modified = true;
            }
            Key::Backspace => {
                if self.col > 0 {
                    self.col -= 1;
                    let at = self.byte_at(self.row, self.col);
                    self.lines[self.row].remove(at);
                    self.modified = true;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                    self.lines[self.row].push_str(&line);
                    self.modified = true;
                }
            }
            Key::Delete => {
                if self.col < self.line_len(self.row) {
                    let at = self.byte_at(self.row, self.col);
                    self.lines[self.row].remove(at);
                    self.modified = true;
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                    self.modified = true;
                }
            }
            Key::Up => self.row = self.row.saturating_sub(1),
            Key::Down => self.row = (self.row + 1).min(self.lines.len() - 1),
            Key::PageUp => self.row = self.row.saturating_sub(rows),
            Key::PageDown => self.row = (self.row + rows).min(self.lines.len() - 1),
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
            }
            Key::Right => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            Key::Home => self.col = 0,
            Key::End => self.col = self.line_len(self.row),
            _ => {}
        }
        self.col = self.col.min(self.line_len(self.row));
    }

    fn insert(&mut self, ch: char) {
        let at = self.byte_at(self.row, self.col);
        self.lines[self.row].insert(at, ch);
        self.col += 1;
        self.modified = true;
    }

    fn scroll(&mut self, rows: usize, cols: usize) {
        if self.row < self.top {
            self.top = self.row;
        } else if self.row >= self.top + rows {
            self.top = self.row + 1 - rows;
        }
        let column = visual_column(&self.lines[self.row], self.col);
        if column < self.left {
            self.left = column;
        } else if column >= self.left + cols {
            self.left = column + 1 - cols;
        }
    }

    fn draw(&self, canvas: &mut Canvas) {
        let (cols, rows) = (canvas.cols, canvas.rows);
        let text_rows = rows.saturating_sub(2);
        canvas.fill(0, 0, cols, rows, ' ', ink::FILE, ink::PANEL);
        canvas.fill(0, 0, cols, 1, ' ', ink::CURSOR_FG, ink::CURSOR_BG);
        let status = match &self.note {
            Some(note) => format!("{note} "),
            None => format!("стр {}/{} · кол {} ", self.row + 1, self.lines.len(), self.col + 1),
        };
        let status_len = status.chars().count().min(cols);
        let title = if self.modified { format!("{} *", self.path) } else { self.path.clone() };
        canvas.text(0, 1, cols.saturating_sub(status_len + 2), &title, ink::CURSOR_FG, ink::CURSOR_BG);
        canvas.right(0, 0, cols, &status, ink::CURSOR_FG, ink::CURSOR_BG);
        for i in 0..text_rows {
            let Some(line) = self.lines.get(self.top + i) else {
                break;
            };
            draw_bytes(canvas, 1 + i, line.as_bytes(), self.left, ink::FILE, ink::PANEL);
        }
        keybar(canvas, rows.saturating_sub(1), &EDIT_KEYS);
        let column = visual_column(&self.lines[self.row], self.col);
        canvas.cursor = if self.row >= self.top && column >= self.left {
            Some((1 + self.row - self.top, column - self.left))
        } else {
            None
        };
    }

    /// Записать файл. Возвращает число байт.
    fn save(&mut self) -> Result<usize, String> {
        let total: usize = self.lines.iter().map(|line| line.len() + 1).sum();
        let mut data = Vec::new();
        if data.try_reserve_exact(total).is_err() {
            return Err(String::from("Не хватило памяти, чтобы сохранить файл."));
        }
        for (index, line) in self.lines.iter().enumerate() {
            data.extend_from_slice(line.as_bytes());
            if index + 1 < self.lines.len() || self.newline_at_end {
                data.push(b'\n');
            }
        }
        let fd = open_write(&self.path, true, true);
        if fd < 0 {
            return Err(format!("Не удалось записать «{}»: {}", self.path, error_text(fd)));
        }
        let written = write(fd, &data);
        close(fd);
        if written != data.len() as i64 {
            let code = if written < 0 { written } else { ERR_NO_SPACE };
            return Err(format!("Записано не всё: {}", error_text(code)));
        }
        self.modified = false;
        Ok(data.len())
    }
}
