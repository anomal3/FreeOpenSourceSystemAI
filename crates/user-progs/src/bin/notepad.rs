//! Блокнот: свой текстовый редактор с окном (фаза 70b, макет FossAi 02.11).
//!
//! # Зачем свой
//!
//! В системе есть `mc` с редактором — но в терминале и с клавишами, которые
//! человек с Windows не знает. Блокнот — первое, что он ищет в «Пуске»: открыть
//! файл, поправить строку, сохранить.
//!
//! # Что умеет
//!
//! * Полоса меню «Файл / Правка / Вид / Синтаксис» с сочетаниями справа.
//! * Открыть, сохранить, сохранить как (Ctrl+O, Ctrl+S, Ctrl+Shift+S) — общим
//!   окном «Открыть» / «Сохранить как» (`mini_ui::filedlg`, 20.11: быстрый
//!   доступ, адрес, таблица, тип файла; с 20.11 — отдельным модальным окном
//!   стола `/bin/filedlg`); создать (Ctrl+N). Несохранённое — вопрос
//!   «Сохранить изменения?» перед «Создать», «Открыть» и закрытием окна.
//! * Выделение Shift+стрелками, Shift+Home/End, Ctrl+A; Вырезать, Копировать,
//!   Вставить через общий буфер обмена стола (`SYS_CLIPBOARD`: тот же, что у
//!   терминала); отменить и повторить (Ctrl+Z, Ctrl+Y).
//! * Подсветка синтаксиса, как в Notepad++ (`mini_ui::syntax`): язык — по
//!   расширению, сменить — меню «Синтаксис» или щелчок по имени языка в строке
//!   состояния.
//!
//! * Шрифт и размер (Вид → Шрифт…): шрифты системы из `/usr/share/fonts/fossai`
//!   (TrueType, растеризуются на лету — `crates/ttf`) или встроенный
//!   моноширинный; выбор помнится в `~/.config/notepad.cfg`.
//!
//! Перенос строк выключен: длинная строка уезжает вправо вслед за кареткой.
//! Текст — UTF-8; файл, который не UTF-8, не открывается, а не превращается в
//! мусор при сохранении.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use core::cell::RefCell;
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::syntax::{self, Kind, Lang};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_abi::{Dirent, KIND_DIRECTORY, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use user_progs::app::{self, App, Spec};
use user_progs::filedlg::{Dialog, Filter, Options, Outcome};
use user_progs::{
    Args, SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN,
    WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, clipboard_get, clipboard_set, close, file_size, open, open_write,
    println, read, readdir_raw, write,
};

const SPEC: Spec = Spec { name: "notepad", title: "Notepad", period_ms: 1_000 };

/// Самый большой файл, который блокнот откроет: четыре мегабайта. Больше —
/// это уже не заметка, и держать его строками в куче программы незачем.
const MAX_FILE: usize = 4 * 1024 * 1024;

/// Сколько памяти может занять история отмены (снимки текста).
const UNDO_BYTES: usize = 8 * 1024 * 1024;
/// Сколько шагов отмены, не больше.
const UNDO_STEPS: usize = 200;

const ENTER: u32 = '\n' as u32;
const TAB: u32 = '\t' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const CTRL_N: u32 = 0x0E;
const CTRL_O: u32 = 0x0F;
const CTRL_S: u32 = 0x13;

const MENU_H: u32 = 32;
const STATUS_H: u32 = 28;
const GUTTER_CHARS: usize = 4;
/// Строка выпадающего меню и окна «Шрифт».
const ITEM_H: u32 = 30;
/// Где шрифты системы.
const FONTS_DIR: &str = "/usr/share/fonts/fossai";
/// Размеры на выбор, точки.
const SIZES: [u32; 10] = [10, 11, 12, 13, 14, 16, 18, 20, 24, 28];
/// Самый большой файл шрифта.
const MAX_FONT: usize = 8 * 1024 * 1024;
const F10: u32 = user_abi::WIN_KEY_F1 + 9;

/// Шрифт TrueType в работе: растры глифов для текущего размера.
struct Face {
    font: ttf::Font,
    px: f32,
    cache: BTreeMap<char, ttf::Bitmap>,
}

impl Face {
    fn load(path: &str) -> Option<Self> {
        let data = read_bytes(path, MAX_FONT)?;
        Some(Self { font: ttf::Font::parse(data)?, px: 0.0, cache: BTreeMap::new() })
    }

    /// Размер в точках экрана; другой — растры заново.
    fn set_px(&mut self, px: f32) {
        if self.px != px {
            self.px = px;
            self.cache.clear();
        }
    }

    fn glyph(&mut self, ch: char) -> &ttf::Bitmap {
        let (font, px) = (&self.font, self.px);
        self.cache.entry(ch).or_insert_with(|| font.rasterize(ch, px))
    }
}

/// Шрифт текста: встроенный моноширинный (`face: None`) или с диска.
struct TextFont {
    face: Option<RefCell<Face>>,
    label: String,
    path: Option<String>,
    size: u32,
}

/// Окно «Шрифт»: список шрифтов, размеры, образец.
struct FontDialog {
    /// Подпись и путь (`None` — встроенный).
    fonts: Vec<(String, Option<String>)>,
    font: usize,
    size: usize,
    /// Стрелки ходят по шрифтам (0) или размерам (1); Tab — переключить.
    focus: u8,
    preview: Option<RefCell<Face>>,
}

/// Место в тексте: строка и номер **символа** в ней (не байта).
type Pos = (usize, usize);

/// Снимок для отмены.
struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

/// Чем была последняя правка: набор подряд — один шаг отмены.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Typing,
    Deleting,
    Other,
}

/// Меню полосы.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Menu {
    File,
    Edit,
    View,
    Syntax,
}

const MENUS: [(Menu, &str); 4] = [(Menu::File, "Файл"), (Menu::Edit, "Правка"), (Menu::View, "Вид"), (Menu::Syntax, "Синтаксис")];

/// Команда меню.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    LineNumbers,
    Font,
    Find,
    FindNext,
    Replace,
    Wrap,
    Lang(usize),
}

/// Пункт выпадающего меню: подпись, сочетание, команда, группа.
struct Item {
    label: String,
    keys: &'static str,
    cmd: Cmd,
    group: u8,
}

/// Что сделать после ответа на «Сохранить изменения?».
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    New,
    Open,
    Quit,
}

struct Editor {
    /// Строки без перевода строки на конце.
    lines: Vec<String>,
    row: usize,
    col: usize,
    /// Второй конец выделения; `None` — выделения нет.
    anchor: Option<Pos>,
    /// Первая показанная строка и первый показанный столбец.
    top: usize,
    left: usize,
    path: Option<String>,
    dirty: bool,
    note: Option<String>,
    /// Сколько строк и столбцов помещается — запоминается при отрисовке, чтобы
    /// PageDown листал ровно на экран.
    page_rows: core::cell::Cell<usize>,
    page_cols: core::cell::Cell<usize>,
    lang: Lang,
    /// Открытое меню полосы и подсвеченный пункт (`usize::MAX` — none:
    /// the pointer left the rows).
    menu: Option<(Menu, usize)>,
    /// Menu-bar title under the pointer (25.11: hover on the menu bar).
    hot_head: Option<usize>,
    /// Открыто меню языков у строки состояния: подсвеченный пункт.
    lang_menu: Option<usize>,
    /// Состояние подсветки на начало каждой строки (см. `state_at`).
    states: core::cell::RefCell<Vec<syntax::State>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    numbers: bool,
    /// Окно «Открыть» / «Сохранить как» — отдельная программа `/bin/filedlg`
    /// над нашим окном (20.11) — и что это: сохранение (`true`).
    dialog: Option<Dialog>,
    dialog_save: bool,
    /// Вопрос «Сохранить изменения?» и что сделать после ответа.
    ask_save: Option<Then>,
    quit: bool,
    home: String,
    font: TextFont,
    font_dialog: Option<FontDialog>,
    /// Перенос по словам (Вид).
    wrap: bool,
    /// Экранные строки последней отрисовки: строка текста, кусок, верх.
    screen_map: RefCell<Vec<(usize, usize, usize, i32)>>,
    /// Панель «Найти» / «Заменить».
    find: Option<FindBar>,
    /// Выделение мышью (24.11): зажатая кнопка, модификаторы последнего
    /// щелчка и прошлый щелчок (время, точка, который по счёту).
    press: Option<Press>,
    click_mods: i32,
    last_click: (u64, i32, i32, u8),
}

/// Панель поиска над текстом (Ctrl+F, Ctrl+H).
struct FindBar {
    what: String,
    with: String,
    /// С полем замены.
    replace: bool,
    /// Курсор в поле замены (иначе — в поле поиска).
    in_with: bool,
}

/// Высота панели поиска.
const FIND_H: u32 = 44;

/// Чем мышь выделяет (24.11): знаками, словами (двойной щелчок), строками
/// (тройной).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

/// Левая кнопка зажата в тексте: чем выделяем, что выделил щелчок (начало и
/// конец), и где указатель — для прокрутки, пока он над или под текстом.
#[derive(Clone, Copy)]
struct Press {
    unit: Unit,
    origin: (Pos, Pos),
    area: Rect,
    ctx: Ctx,
    x: i32,
    y: i32,
}

/// Двойной и тройной щелчок: не дольше этого между щелчками ...
const MULTI_CLICK_MS: u64 = 500;
/// ... и не дальше этого (точки макета).
const MULTI_CLICK_PX: u32 = 4;
const F3: u32 = user_abi::WIN_KEY_F1 + 2;

impl Editor {
    fn new(path: Option<&str>) -> Self {
        let mut editor = Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            anchor: None,
            top: 0,
            left: 0,
            path: None,
            dirty: false,
            note: None,
            page_rows: core::cell::Cell::new(20),
            page_cols: core::cell::Cell::new(80),
            lang: Lang::Plain,
            menu: None,
            hot_head: None,
            lang_menu: None,
            states: core::cell::RefCell::new(Vec::new()),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::None,
            numbers: true,
            dialog: None,
            dialog_save: false,
            ask_save: None,
            quit: false,
            home: user_progs::filedlg::home_dir(),
            font: TextFont { face: None, label: String::from(BUILTIN_FONT), path: None, size: 12 },
            font_dialog: None,
            wrap: false,
            screen_map: RefCell::new(Vec::new()),
            find: None,
            press: None,
            click_mods: 0,
            last_click: (0, 0, 0, 0),
        };
        editor.load_font_settings();
        if let Some(path) = path {
            editor.load(path);
        }
        editor
    }

    fn name(&self) -> &str {
        self.path.as_deref().map_or("Без имени", |path| path.rsplit('/').next().unwrap_or(path))
    }

    fn title(&self) -> String {
        format!("{}{}", if self.dirty { "● " } else { "" }, self.name())
    }

    // -----------------------------------------------------------------------
    // Подсветка
    // -----------------------------------------------------------------------

    /// Строка `row` изменилась: состояния подсветки после неё — заново.
    fn touch(&self, row: usize) {
        self.states.borrow_mut().truncate(row + 1);
    }

    /// Состояние подсветки на начало строки `row`.
    fn state_at(&self, row: usize) -> syntax::State {
        let mut states = self.states.borrow_mut();
        if states.is_empty() {
            states.push(false);
        }
        while states.len() <= row && states.len() <= self.lines.len() {
            let index = states.len() - 1;
            let mut state = states[index];
            if let Some(line) = self.lines.get(index) {
                let _ = syntax::spans(self.lang, line, &mut state);
            }
            states.push(state);
        }
        states.get(row).copied().unwrap_or(false)
    }

    fn set_lang(&mut self, lang: Lang) {
        if self.lang != lang {
            self.lang = lang;
            self.states.borrow_mut().clear();
            println(&format!("notepad: syntax {}", lang.name()));
        }
    }

    // -----------------------------------------------------------------------
    // Файл
    // -----------------------------------------------------------------------

    fn load(&mut self, path: &str) {
        match read_file(path) {
            Ok(text) => {
                self.lines = text.split('\n').map(|line| line.trim_end_matches('\r').to_string()).collect();
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.path = Some(path.to_string());
                self.reset_view();
                self.set_lang(Lang::detect(path));
                self.note = Some(format!("Открыт {path}: строк {}", self.lines.len()));
                println(&format!("notepad: opened {path}, {} line(s)", self.lines.len()));
            }
            Err(why) => {
                // Несуществующий файл — это новый файл с таким именем: так
                // ведёт себя Блокнот Windows, и `notepad заметка.txt` должно
                // работать и тогда, когда заметки ещё нет.
                if why == "нет такого файла" {
                    self.lines = vec![String::new()];
                    self.path = Some(path.to_string());
                    self.reset_view();
                    self.set_lang(Lang::detect(path));
                    self.note = Some(format!("Новый файл {path}"));
                    println(&format!("notepad: new file {path}"));
                } else {
                    self.note = Some(format!("Не открыть {path}: {why}"));
                    println(&format!("notepad: cannot open {path}: {why}"));
                }
            }
        }
    }

    fn reset_view(&mut self) {
        self.row = 0;
        self.col = 0;
        self.anchor = None;
        self.top = 0;
        self.left = 0;
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = EditKind::None;
        self.touch(0);
    }

    /// Сохранить. `false` — не вышло (или имени нет и открыто окно «Сохранить как»).
    fn save(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            self.open_dialog(true);
            return false;
        };
        let mut text = self.lines.join("\n");
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let fd = open_write(&path, true, true);
        if fd < 0 {
            self.note = Some(format!("Не сохранить {path}: {}", error_text(fd)));
            println(&format!("notepad: cannot save {path}: {fd}"));
            return false;
        }
        let bytes = text.as_bytes();
        let mut done = 0usize;
        while done < bytes.len() {
            let step = write(fd, &bytes[done..]);
            if step <= 0 {
                break;
            }
            done += step as usize;
        }
        close(fd);
        if done < bytes.len() {
            self.note = Some(format!("{path}: записано {done} из {} байт", bytes.len()));
            return false;
        }
        self.dirty = false;
        self.note = Some(format!("Сохранено: {path}, {} байт", bytes.len()));
        println(&format!("notepad: saved {path}, {} byte(s)", bytes.len()));
        true
    }

    fn new_file(&mut self) {
        self.lines = vec![String::new()];
        self.path = None;
        self.reset_view();
        self.set_lang(Lang::Plain);
        self.note = Some(String::from("Новый документ"));
    }

    /// Сделать `then`, а если есть несохранённое — сначала спросить.
    fn guarded(&mut self, then: Then) {
        if self.dirty {
            self.ask_save = Some(then);
            return;
        }
        self.proceed(then);
    }

    fn proceed(&mut self, then: Then) {
        match then {
            Then::New => self.new_file(),
            Then::Open => self.open_dialog(false),
            Then::Quit => self.quit = true,
        }
    }

    // -----------------------------------------------------------------------
    // Окно «Открыть» / «Сохранить как»
    // -----------------------------------------------------------------------

    fn open_dialog(&mut self, save: bool) {
        let filters = vec![
            Filter::new("Текстовые файлы", &["txt"]),
            Filter::new("Исходный код", &["rs", "c", "h", "py", "sh", "lua", "cs", "js", "json", "toml"]),
            Filter::all(),
        ];
        let ext = self.path.as_deref().map(|path| path.rsplit('/').next().unwrap_or(path)).and_then(|name| name.rsplit_once('.')).map(|(_, ext)| ext.to_ascii_lowercase());
        // Новый документ или .txt — «Текстовые файлы» (к имени без
        // расширения допишется .txt); остальное — «Все файлы».
        let filter = if save && matches!(ext.as_deref(), None | Some("txt")) { 0 } else { 2 };
        let mut options = if save {
            Options::save(&self.path.as_deref().map_or_else(|| String::from("Без имени.txt"), |path| path.rsplit('/').next().unwrap_or(path).to_string()))
        } else {
            Options::open()
        }
        .filters(filters)
        .filter(filter);
        // Файл уже есть на диске — окно в его папке; иначе — последняя папка
        // или «Документы» (решает окно).
        if let Some(path) = &self.path {
            options = options.dir(&parent_of(path));
        }
        // Окно уже открыто — второе не нужно (стол и так держит его над нами).
        if self.dialog.is_some() {
            return;
        }
        match Dialog::start(&options, "notepad") {
            Ok(dialog) => {
                println(&format!("notepad: {} dialog opened", if save { "save" } else { "open" }));
                self.dialog = Some(dialog);
                self.dialog_save = save;
            }
            Err(why) => {
                println(&format!("notepad: FAILED file dialog: {why}"));
                self.note = Some(format!("Окно выбора файла не открылось: {why}"));
            }
        }
    }

    /// Ответ окна выбора файла.
    fn dialog_done(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pending => {}
            Outcome::Cancelled => self.dialog = None,
            Outcome::Chosen(path) => {
                self.dialog = None;
                if self.dialog_save {
                    self.set_lang(Lang::detect(&path));
                    self.path = Some(path);
                    self.save();
                } else {
                    self.load(&path);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Правка
    // -----------------------------------------------------------------------

    fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |line| line.chars().count())
    }

    /// Байтовое смещение символа `col` в строке.
    fn byte_at(line: &str, col: usize) -> usize {
        line.char_indices().nth(col).map_or(line.len(), |(at, _)| at)
    }

    /// Запомнить текст для отмены: перед первой правкой серии.
    fn checkpoint(&mut self, kind: EditKind) {
        if kind == self.last_edit && kind != EditKind::Other {
            return;
        }
        self.last_edit = kind;
        self.undo.push(Snapshot { lines: self.lines.clone(), row: self.row, col: self.col });
        self.redo.clear();
        // Старые шаги уходят, когда история выросла сверх меры.
        let mut bytes: usize = self.undo.iter().map(|snap| snap.lines.iter().map(String::len).sum::<usize>()).sum();
        while (self.undo.len() > UNDO_STEPS || bytes > UNDO_BYTES) && self.undo.len() > 1 {
            let gone = self.undo.remove(0);
            bytes -= gone.lines.iter().map(String::len).sum::<usize>();
        }
    }

    fn restore(&mut self, redo: bool) {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(snap) = from.pop() else {
            return;
        };
        to.push(Snapshot { lines: core::mem::replace(&mut self.lines, snap.lines), row: self.row, col: self.col });
        self.row = snap.row.min(self.lines.len() - 1);
        self.col = snap.col.min(self.line_len(self.row));
        self.anchor = None;
        self.dirty = true;
        self.last_edit = EditKind::None;
        self.touch(0);
        println(&format!("notepad: {}", if redo { "redo" } else { "undo" }));
    }

    /// Выделение по порядку: начало, конец. `None` — пусто.
    fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        let caret = (self.row, self.col);
        if anchor == caret {
            return None;
        }
        Some(if anchor < caret { (anchor, caret) } else { (caret, anchor) })
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        let mut out = String::new();
        for row in start.0..=end.0 {
            let line = &self.lines[row];
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { line.chars().count() };
            out.extend(line.chars().skip(from).take(to.saturating_sub(from)));
            if row != end.0 {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Убрать выделенное; каретка — на его начало. `true` — было что убрать.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = {
            let line = &self.lines[end.0];
            line[Self::byte_at(line, end.1)..].to_string()
        };
        let head_end = Self::byte_at(&self.lines[start.0], start.1);
        self.lines[start.0].truncate(head_end);
        self.lines[start.0].push_str(&tail);
        self.lines.drain(start.0 + 1..=end.0);
        self.row = start.0;
        self.col = start.1;
        self.anchor = None;
        self.dirty = true;
        self.touch(start.0);
        true
    }

    /// Вставить текст в каретку (с переводами строк).
    fn insert_text(&mut self, text: &str) {
        let text = text.replace('\r', "");
        let line = &mut self.lines[self.row];
        let at = Self::byte_at(line, self.col);
        let rest = line.split_off(at);
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            self.lines[self.row].push_str(first);
            self.col += first.chars().count();
        }
        for part in parts {
            self.row += 1;
            self.lines.insert(self.row, part.to_string());
            self.col = part.chars().count();
        }
        self.lines[self.row].push_str(&rest);
        self.dirty = true;
        self.touch(self.row.saturating_sub(text.matches('\n').count()));
    }

    fn insert(&mut self, ch: char) {
        let kind = if ch == ' ' { EditKind::Other } else { EditKind::Typing };
        self.checkpoint(kind);
        self.delete_selection();
        let line = &mut self.lines[self.row];
        let at = Self::byte_at(line, self.col);
        line.insert(at, ch);
        self.col += 1;
        self.dirty = true;
        self.touch(self.row);
    }

    fn newline(&mut self) {
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        let line = &mut self.lines[self.row];
        let at = Self::byte_at(line, self.col);
        let rest = line.split_off(at);
        // Отступ новой строки — как у прежней: в списках и коде это то, что
        // человек набрал бы следующим.
        let indent: String = self.lines[self.row].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        self.row += 1;
        self.col = indent.chars().count();
        self.lines.insert(self.row, indent + &rest);
        self.dirty = true;
        self.touch(self.row - 1);
    }

    fn backspace(&mut self) {
        self.checkpoint(EditKind::Deleting);
        if self.delete_selection() {
            return;
        }
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let at = Self::byte_at(line, self.col - 1);
            line.remove(at);
            self.col -= 1;
            self.dirty = true;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&line);
            self.dirty = true;
        }
        self.touch(self.row);
    }

    fn delete(&mut self) {
        self.checkpoint(EditKind::Deleting);
        if self.delete_selection() {
            return;
        }
        if self.col < self.line_len(self.row) {
            let line = &mut self.lines[self.row];
            let at = Self::byte_at(line, self.col);
            line.remove(at);
            self.dirty = true;
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
            self.dirty = true;
        }
        self.touch(self.row);
    }

    fn copy(&mut self) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        let count = text.chars().count();
        if clipboard_set(&text) {
            println(&format!("notepad: copied {count} char(s)"));
            self.note = Some(format!("Скопировано знаков: {count}"));
        }
        true
    }

    fn cut(&mut self) {
        if self.copy() {
            self.checkpoint(EditKind::Other);
            self.delete_selection();
        }
    }

    fn paste(&mut self) {
        let text = clipboard_get();
        if text.is_empty() {
            return;
        }
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        self.insert_text(&text);
        println(&format!("notepad: pasted {} char(s)", text.chars().count()));
    }

    fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.row = self.lines.len() - 1;
        self.col = self.line_len(self.row);
    }

    /// Каретку — в новое место; `extend` — тянуть выделение (Shift).
    fn move_caret(&mut self, row: usize, col: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some((self.row, self.col));
            }
        } else {
            self.anchor = None;
        }
        self.row = row.min(self.lines.len() - 1);
        self.col = col.min(self.line_len(self.row));
        self.last_edit = EditKind::None;
    }

    /// Прокрутить так, чтобы каретка была видна.
    fn follow(&mut self) {
        let rows = self.page_rows.get().max(1);
        let cols = self.page_cols.get().max(1);
        if self.row < self.top {
            self.top = self.row;
        } else if self.row >= self.top + rows {
            self.top = self.row + 1 - rows;
        }
        if self.wrap {
            self.left = 0;
        } else if self.col < self.left {
            self.left = self.col;
        } else if self.col >= self.left + cols {
            self.left = self.col + 1 - cols;
        }
    }

    // -----------------------------------------------------------------------
    // Меню
    // -----------------------------------------------------------------------

    fn items(&self, menu: Menu) -> Vec<Item> {
        let item = |label: &str, keys: &'static str, cmd: Cmd, group: u8| Item { label: label.to_string(), keys, cmd, group };
        match menu {
            Menu::File => vec![
                item("Создать", "Ctrl+N", Cmd::New, 0),
                item("Открыть…", "Ctrl+O", Cmd::Open, 0),
                item("Сохранить", "Ctrl+S", Cmd::Save, 1),
                item("Сохранить как…", "Ctrl+Shift+S", Cmd::SaveAs, 1),
                item("Выход", "Alt+F4", Cmd::Exit, 2),
            ],
            Menu::Edit => vec![
                item("Отменить", "Ctrl+Z", Cmd::Undo, 0),
                item("Повторить", "Ctrl+Y", Cmd::Redo, 0),
                item("Вырезать", "Ctrl+X", Cmd::Cut, 1),
                item("Копировать", "Ctrl+C", Cmd::Copy, 1),
                item("Вставить", "Ctrl+V", Cmd::Paste, 1),
                item("Удалить", "Del", Cmd::Delete, 1),
                item("Выделить всё", "Ctrl+A", Cmd::SelectAll, 2),
                item("Найти…", "Ctrl+F", Cmd::Find, 3),
                item("Найти далее", "F3", Cmd::FindNext, 3),
                item("Заменить…", "Ctrl+H", Cmd::Replace, 3),
            ],
            Menu::View => vec![
                item(if self.numbers { "✓ Номера строк" } else { "   Номера строк" }, "", Cmd::LineNumbers, 0),
                item(if self.wrap { "✓ Перенос по словам" } else { "   Перенос по словам" }, "", Cmd::Wrap, 0),
                item("Шрифт…", "", Cmd::Font, 1),
            ],
            Menu::Syntax => Lang::ALL
                .iter()
                .enumerate()
                .map(|(index, lang)| {
                    let mark = if *lang == self.lang { "● " } else { "   " };
                    item(&format!("{mark}{}", lang.name()), "", Cmd::Lang(index), u8::from(index > 0))
                })
                .collect(),
        }
    }

    fn run(&mut self, cmd: Cmd) {
        self.menu = None;
        match cmd {
            Cmd::New => self.guarded(Then::New),
            Cmd::Open => self.guarded(Then::Open),
            Cmd::Save => {
                self.save();
            }
            Cmd::SaveAs => self.open_dialog(true),
            Cmd::Exit => self.guarded(Then::Quit),
            Cmd::Undo => self.restore(false),
            Cmd::Redo => self.restore(true),
            Cmd::Cut => self.cut(),
            Cmd::Copy => {
                self.copy();
            }
            Cmd::Paste => self.paste(),
            Cmd::Delete => self.delete(),
            Cmd::SelectAll => self.select_all(),
            Cmd::LineNumbers => self.numbers = !self.numbers,
            Cmd::Font => self.open_font_dialog(),
            Cmd::Find | Cmd::Replace => {
                // Выделенное — сразу в поле поиска, как в Блокноте Windows.
                let what = self.selected_text().filter(|text| !text.contains('\n')).or_else(|| self.find.as_ref().map(|bar| bar.what.clone())).unwrap_or_default();
                let with = self.find.as_ref().map(|bar| bar.with.clone()).unwrap_or_default();
                self.find = Some(FindBar { what, with, replace: cmd == Cmd::Replace, in_with: false });
            }
            Cmd::FindNext => {
                if self.find.as_ref().is_some_and(|bar| !bar.what.is_empty()) {
                    self.find_next();
                } else {
                    self.run(Cmd::Find);
                }
            }
            Cmd::Wrap => {
                self.wrap = !self.wrap;
                self.left = 0;
                println(&format!("notepad: wrap {}", if self.wrap { "on" } else { "off" }));
            }
            Cmd::Lang(index) => self.set_lang(Lang::ALL[index]),
        }
        self.follow();
    }
}

impl Editor {
    fn settings_path(&self) -> String {
        format!("{}/.config/notepad.cfg", self.home)
    }

    /// Прочитать выбранный шрифт и размер.
    fn load_font_settings(&mut self) {
        let Some(text) = read_bytes(&self.settings_path(), 4096).and_then(|bytes| String::from_utf8(bytes).ok()) else {
            return;
        };
        let size = sysconf::value(&text, "size").and_then(|value| value.trim().parse::<u32>().ok()).filter(|size| (8..=48).contains(size)).unwrap_or(12);
        let path = sysconf::value(&text, "font").map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        let label = path.as_deref().map_or_else(|| String::from(BUILTIN_FONT), |path| font_label(path.rsplit('/').next().unwrap_or(path)));
        self.set_font(label, path, size);
    }

    fn set_font(&mut self, label: String, path: Option<String>, size: u32) {
        let face = path.as_deref().and_then(Face::load);
        // Не открылся — встроенный: текст обязан остаться видимым.
        let (label, path) = if path.is_some() && face.is_none() { (String::from(BUILTIN_FONT), None) } else { (label, path) };
        println(&format!("notepad: font {label} {size}"));
        self.font = TextFont { face: face.map(RefCell::new), label, path, size };
    }

    fn save_font_settings(&self) {
        let dir = format!("{}/.config", self.home);
        let _ = user_progs::mkdir(&dir, 0o755);
        let text = format!("font={}\nsize={}\n", self.font.path.as_deref().unwrap_or(""), self.font.size);
        let fd = open_write(&self.settings_path(), true, true);
        if fd >= 0 {
            write(fd, text.as_bytes());
            close(fd);
        }
    }

    fn open_font_dialog(&mut self) {
        let fonts = system_fonts();
        let font = fonts.iter().position(|(_, path)| *path == self.font.path).unwrap_or(0);
        let size = SIZES.iter().position(|size| *size == self.font.size).unwrap_or(2);
        let mut dialog = FontDialog { fonts, font, size, focus: 0, preview: None };
        dialog.load_preview();
        println(&format!("notepad: font dialog, {} font(s)", dialog.fonts.len()));
        self.font_dialog = Some(dialog);
    }

    fn font_accept(&mut self) {
        let Some(dialog) = self.font_dialog.take() else {
            return;
        };
        let (label, path) = dialog.fonts[dialog.font].clone();
        self.set_font(label, path, SIZES[dialog.size]);
        self.save_font_settings();
        self.note = Some(format!("Шрифт: {} {}", self.font.label, self.font.size));
    }

    fn font_key(&mut self, code: u32) {
        let Some(dialog) = self.font_dialog.as_mut() else {
            return;
        };
        match code {
            ESCAPE => self.font_dialog = None,
            ENTER => self.font_accept(),
            TAB | WIN_KEY_LEFT | WIN_KEY_RIGHT => dialog.focus = 1 - dialog.focus.min(1),
            WIN_KEY_UP | WIN_KEY_DOWN => {
                let down = code == WIN_KEY_DOWN;
                if dialog.focus == 0 {
                    let last = dialog.fonts.len() - 1;
                    dialog.font = if down { (dialog.font + 1).min(last) } else { dialog.font.saturating_sub(1) };
                    dialog.load_preview();
                } else {
                    dialog.size = if down { (dialog.size + 1).min(SIZES.len() - 1) } else { dialog.size.saturating_sub(1) };
                }
            }
            _ => {}
        }
    }

    fn font_click(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) {
        let plan = font_plan(ctx, area);
        if plan.ok.contains(x, y) {
            self.font_accept();
            return;
        }
        if plan.cancel.contains(x, y) {
            self.font_dialog = None;
            return;
        }
        let Some(dialog) = self.font_dialog.as_mut() else {
            return;
        };
        let row = |list: Rect| ((y - list.y - ctx.px(2) as i32).max(0) as u32 / ctx.px(ITEM_H)) as usize;
        if plan.fonts.contains(x, y) {
            let index = row(plan.fonts) + font_first(ctx, &plan, dialog.font);
            if index < dialog.fonts.len() {
                dialog.font = index;
                dialog.focus = 0;
                dialog.load_preview();
            }
        } else if plan.sizes.contains(x, y) {
            let index = row(plan.sizes);
            if index < SIZES.len() {
                dialog.size = index;
                dialog.focus = 1;
            }
        }
    }

    fn draw_font_dialog(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let Some(dialog) = &self.font_dialog else {
            return;
        };
        let p = ctx.palette;
        draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 90);
        let plan = font_plan(ctx, area);
        let inner = kit::card(ctx, s, plan.card);
        let pad = ctx.px(20) as i32;
        paint::text(inner, s, Role::Title, plan.card.x + pad, plan.card.y + pad, "Шрифт", p.ink);
        for (list, focused) in [(plan.fonts, dialog.focus == 0), (plan.sizes, dialog.focus == 1)] {
            paint::sunk(inner, s, list, ctx.px(6));
            if focused {
                draw::rounded_stroke(s, list, ctx.px(6), p.acc, 255);
            }
        }
        let row_rect = |list: Rect, index: usize| Rect::new(list.x + ctx.px(4) as i32, list.y + ctx.px(2) as i32 + (index as u32 * ctx.px(ITEM_H)) as i32, list.w - ctx.px(8), ctx.px(ITEM_H));
        let first = font_first(ctx, &plan, dialog.font);
        let visible = (plan.fonts.h / ctx.px(ITEM_H)) as usize;
        for (index, (label, _)) in dialog.fonts.iter().enumerate().skip(first).take(visible) {
            let row = row_rect(plan.fonts, index - first);
            if index == dialog.font {
                draw::rounded(s, row, ctx.px(4), p.acc, 60);
            }
            paint::text_clipped(inner, s, Role::Body, row.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, row), row.w - ctx.px(20), label, p.ink);
        }
        for (index, size) in SIZES.iter().enumerate().take((plan.sizes.h / ctx.px(ITEM_H)) as usize) {
            let row = row_rect(plan.sizes, index);
            if index == dialog.size {
                draw::rounded(s, row, ctx.px(4), p.acc, 60);
            }
            paint::text(inner, s, Role::Body, row.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, row), &format!("{size}"), p.ink);
        }
        // Образец — выбранным шрифтом и размером.
        paint::sunk(inner, s, plan.preview, ctx.px(6));
        let sample = "Съешь же ещё этих булок. fn main() { 42 }";
        let size = ctx.px(SIZES[dialog.size]) as f32;
        let clip = Rect::new(plan.preview.x + ctx.px(8) as i32, plan.preview.y, plan.preview.w - ctx.px(16), plan.preview.h);
        match &dialog.preview {
            Some(face) => {
                let mut face = face.borrow_mut();
                face.set_px(size);
                let metrics = face.font.line_metrics(face.px);
                let base = plan.preview.y + (plan.preview.h as i32 - metrics.line as i32) / 2 + metrics.ascent as i32;
                let mut pen = clip.x as f32;
                for ch in sample.chars() {
                    let glyph = face.glyph(ch);
                    blit_glyph(s, glyph, pen as i32 + glyph.left, base + glyph.top, p.ink, clip);
                    pen += glyph.advance;
                }
            }
            None => {
                paint::text_clipped(inner, s, Role::Mono, clip.x, paint::baseline(ctx, Role::Mono, plan.preview), clip.w, sample, p.ink);
            }
        }
        paint::text_clipped(inner, s, Role::Caption, plan.card.x + pad, plan.ok.y + ctx.px(8) as i32, (plan.ok.x - plan.card.x - pad - ctx.px(8) as i32).max(0) as u32, "Tab — шрифт / размер · Enter — готово", p.ink4);
        paint::button(inner, s, plan.ok, Weight::Primary, "ОК", false);
        paint::button(inner, s, plan.cancel, Weight::Normal, "Отмена", false);
    }
}

impl FontDialog {
    fn load_preview(&mut self) {
        self.preview = self.fonts.get(self.font).and_then(|(_, path)| path.as_deref()).and_then(Face::load).map(RefCell::new);
    }
}

/// Первая показанная строка списка шрифтов: выбранная всегда видна.
fn font_first(ctx: Ctx, plan: &FontPlan, selected: usize) -> usize {
    let visible = (plan.fonts.h / ctx.px(ITEM_H)).max(1) as usize;
    (selected + 1).saturating_sub(visible)
}

/// Окно «Шрифт»: карточка, список шрифтов, размеры, образец, кнопки.
struct FontPlan {
    card: Rect,
    fonts: Rect,
    sizes: Rect,
    preview: Rect,
    ok: Rect,
    cancel: Rect,
}

fn font_plan(ctx: Ctx, area: Rect) -> FontPlan {
    let w = ctx.px(540).min(area.w.saturating_sub(ctx.px(24)));
    let h = ctx.px(470).min(area.h.saturating_sub(ctx.px(24)));
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
    let pad = ctx.px(20) as i32;
    let top = card.y + pad + ctx.px(36) as i32;
    let bw = ctx.px(120);
    let by = card.bottom() - pad - ctx.px(32) as i32;
    let preview = Rect::new(card.x + pad, by - ctx.px(14 + 56) as i32, w - ctx.px(40), ctx.px(56));
    let list_h = (preview.y - ctx.px(12) as i32 - top).max(0) as u32;
    let sizes_w = ctx.px(90);
    let fonts = Rect::new(card.x + pad, top, w - ctx.px(40) - sizes_w - ctx.px(10), list_h);
    let sizes = Rect::new(fonts.right() + ctx.px(10) as i32, top, sizes_w, list_h);
    let cancel = Rect::new(card.right() - pad - bw as i32, by, bw, ctx.px(32));
    let ok = Rect::new(cancel.x - ctx.px(8) as i32 - bw as i32, by, bw, ctx.px(32));
    FontPlan { card, fonts, sizes, preview, ok, cancel }
}

impl Editor {
    /// Высота панели поиска (ноль — закрыта).
    fn find_h(&self, ctx: Ctx) -> u32 {
        if self.find.is_some() { ctx.px(FIND_H) } else { 0 }
    }

    /// Клавиша панели поиска. `false` — не её (пусть разбирает текст).
    fn find_key(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & WIN_MOD_CTRL != 0;
        if ctrl {
            // Ctrl+F / Ctrl+H — переключить вид панели; остальное — тексту.
            return matches!(char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()), Some('f' | 'h')) && {
                let replace = latin == u32::from(b'h');
                if let Some(bar) = self.find.as_mut() {
                    bar.replace = replace;
                    bar.in_with = false;
                }
                true
            };
        }
        let Some(bar) = self.find.as_mut() else {
            return false;
        };
        let field = if bar.in_with { &mut bar.with } else { &mut bar.what };
        match code {
            ESCAPE => self.find = None,
            TAB => {
                if bar.replace {
                    bar.in_with = !bar.in_with;
                }
            }
            ENTER => {
                if bar.in_with {
                    self.replace_one();
                } else {
                    self.find_next();
                }
            }
            BACKSPACE => {
                field.pop();
            }
            code if code < 0x0100_0000 => match char::from_u32(code).filter(|ch| !ch.is_control()) {
                Some(ch) if field.chars().count() < 200 => field.push(ch),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Найти следующее вхождение после каретки (без учёта регистра, по кругу).
    fn find_next(&mut self) -> bool {
        let Some(what) = self.find.as_ref().map(|bar| bar.what.to_lowercase()).filter(|what| !what.is_empty()) else {
            return false;
        };
        let start = self.selection().map_or((self.row, self.col), |(_, end)| end);
        let total = self.lines.len();
        for step in 0..=total {
            let row = (start.0 + step) % total;
            let line: Vec<char> = self.lines[row].to_lowercase().chars().collect();
            let pattern: Vec<char> = what.chars().collect();
            let from = if step == 0 { start.1 } else { 0 };
            let found = (from..line.len().saturating_sub(pattern.len()).saturating_add(1)).find(|at| line.get(*at..*at + pattern.len()) == Some(&pattern[..]));
            if let Some(at) = found {
                self.anchor = Some((row, at));
                self.row = row;
                self.col = at + pattern.len();
                self.follow();
                if self.row < self.top || self.row >= self.top + self.page_rows.get() {
                    self.top = self.row.saturating_sub(self.page_rows.get() / 3);
                }
                println(&format!("notepad: found '{what}' at {}:{}", row + 1, at + 1));
                return true;
            }
        }
        println(&format!("notepad: '{what}' not found"));
        self.note = Some(format!("«{what}» не найдено"));
        false
    }

    /// Заменить найденное (выделенное) и перейти к следующему.
    fn replace_one(&mut self) {
        let Some((what, with)) = self.find.as_ref().map(|bar| (bar.what.to_lowercase(), bar.with.clone())) else {
            return;
        };
        if self.selected_text().is_some_and(|text| text.to_lowercase() == what) {
            self.checkpoint(EditKind::Other);
            self.delete_selection();
            self.insert_text(&with);
            println("notepad: replaced 1");
        }
        self.find_next();
    }

    /// Заменить все вхождения.
    fn replace_all(&mut self) {
        let Some((what, with)) = self.find.as_ref().map(|bar| (bar.what.clone(), bar.with.clone())) else {
            return;
        };
        if what.is_empty() {
            return;
        }
        let lower = what.to_lowercase();
        let pattern: Vec<char> = lower.chars().collect();
        self.checkpoint(EditKind::Other);
        let mut count = 0usize;
        for line in &mut self.lines {
            let chars: Vec<char> = line.chars().collect();
            let low: Vec<char> = line.to_lowercase().chars().collect();
            if low.len() != chars.len() {
                continue;
            }
            let mut out = String::new();
            let mut at = 0;
            while at < chars.len() {
                if low.get(at..at + pattern.len()) == Some(&pattern[..]) {
                    out.push_str(&with);
                    at += pattern.len();
                    count += 1;
                } else {
                    out.push(chars[at]);
                    at += 1;
                }
            }
            *line = out;
        }
        if count > 0 {
            self.dirty = true;
            self.anchor = None;
            self.move_caret(self.row, self.col, false);
            self.touch(0);
        }
        println(&format!("notepad: replaced {count}"));
        self.note = Some(format!("Заменено: {count}"));
    }

    /// Раскладка панели поиска: поле поиска, поле замены, кнопки.
    fn find_plan(&self, ctx: Ctx, plan: &Plan) -> (Rect, Rect, Rect, [Rect; 3]) {
        let bar = Rect::new(plan.text.x, plan.text.y, plan.text.w, ctx.px(FIND_H));
        let pad = ctx.px(8) as i32;
        let h = ctx.px(28);
        let y = bar.y + (bar.h as i32 - h as i32) / 2;
        let field_w = ctx.px(200);
        let what = Rect::new(bar.x + pad, y, field_w, h);
        let with = Rect::new(what.right() + pad, y, field_w, h);
        let face = ctx.face(Role::Body);
        let mut x = if self.find.as_ref().is_some_and(|f| f.replace) { with.right() + pad } else { what.right() + pad };
        let mut buttons = [Rect::EMPTY; 3];
        for (slot, label) in buttons.iter_mut().zip(["Найти далее", "Заменить", "Заменить все"]) {
            let w = face.width(label) + ctx.px(20);
            *slot = Rect::new(x, y, w, h);
            x += w as i32 + ctx.px(4) as i32;
        }
        (bar, what, with, buttons)
    }

    fn draw_find(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let Some(find) = &self.find else {
            return;
        };
        let p = ctx.palette;
        let (bar, what, with, buttons) = self.find_plan(ctx, plan);
        s.fill(bar, ctx.flat(p.panel));
        draw::hline(s, bar.x, bar.bottom() - 1, bar.w, p.line.color, p.line.alpha);
        let inner = ctx.on(theme::panel_bg());
        let fields: &[(Rect, &str, &str, bool)] = &[(what, &find.what, "Найти", !find.in_with), (with, &find.with, "Заменить на", find.in_with)];
        for (index, (rect, text, hint, focused)) in fields.iter().enumerate() {
            if index == 1 && !find.replace {
                continue;
            }
            paint::sunk(inner, s, *rect, ctx.px(4));
            if *focused {
                draw::rounded_stroke(s, *rect, ctx.px(4), p.acc, 255);
            }
            let tx = rect.x + ctx.px(8) as i32;
            let room = rect.w - ctx.px(16);
            if text.is_empty() {
                paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(ctx, Role::Body, *rect), room, hint, p.ink5);
            } else {
                paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(ctx, Role::Body, *rect), room, text, p.ink);
            }
            if *focused {
                let w = ctx.face(Role::Body).width(text).min(room);
                s.fill(Rect::new(tx + w as i32 + 1, rect.y + ctx.px(6) as i32, ctx.px(2), rect.h - ctx.px(12)), p.acc);
            }
        }
        for (index, (rect, label)) in buttons.iter().zip(["Найти далее", "Заменить", "Заменить все"]).enumerate() {
            if index > 0 && !find.replace {
                continue;
            }
            paint::button(inner, s, *rect, if index == 0 { Weight::Primary } else { Weight::Normal }, label, false);
        }
    }

    fn find_click(&mut self, ctx: Ctx, plan: &Plan, x: i32) -> bool {
        let (_, what, with, buttons) = self.find_plan(ctx, plan);
        let replace = self.find.as_ref().is_some_and(|bar| bar.replace);
        if let Some(bar) = self.find.as_mut() {
            if what.x <= x && x < what.right() {
                bar.in_with = false;
                return true;
            }
            if replace && with.x <= x && x < with.right() {
                bar.in_with = true;
                return true;
            }
        }
        match buttons.iter().position(|rect| rect.x <= x && x < rect.right()) {
            Some(0) => {
                self.find_next();
            }
            Some(1) if replace => self.replace_one(),
            Some(2) if replace => self.replace_all(),
            _ => return false,
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    bar: Rect,
    heads: [Rect; 4],
    text: Rect,
    status: Rect,
    /// Имя языка справа в строке состояния — щелчок открывает меню языков.
    lang: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let bar = Rect::new(area.x, area.y, area.w, ctx.px(MENU_H));
        let status = Rect::new(area.x, area.bottom() - ctx.px(STATUS_H) as i32, area.w, ctx.px(STATUS_H));
        let text = Rect::new(area.x, bar.bottom(), area.w, (status.y - bar.bottom()).max(0) as u32);
        let mut heads = [Rect::EMPTY; 4];
        let mut x = bar.x + ctx.px(6) as i32;
        for (index, (_, label)) in MENUS.iter().enumerate() {
            let w = ctx.face(Role::Body).width(label) + ctx.px(20);
            heads[index] = Rect::new(x, bar.y + ctx.px(3) as i32, w, bar.h - ctx.px(6));
            x += w as i32;
        }
        let lang_w = ctx.px(150);
        let lang = Rect::new(status.right() - lang_w as i32, status.y, lang_w, status.h);
        Self { bar, heads, text, status, lang }
    }

    /// Меню языков — над строкой состояния, у правого края.
    fn lang_menu(&self, ctx: Ctx, area: Rect) -> (Rect, Vec<kit::Entry<'static>>) {
        let entries: Vec<kit::Entry<'static>> = Lang::ALL
            .iter()
            .map(|lang| kit::Entry { title: lang.name(), icon: None, danger: false, group: u8::from(*lang != Lang::Plain), ..kit::Entry::NONE })
            .collect();
        let size = kit::menu_size(ctx, &entries);
        let rect = kit::place((self.lang.right() - size.0 as i32, self.status.y - size.1 as i32), size, area);
        (rect, entries)
    }
}

/// Где выпадающее меню и его строки.
fn dropdown(ctx: Ctx, area: Rect, head: Rect, items: &[Item]) -> (Rect, Vec<Rect>) {
    let label_w = items.iter().map(|item| ctx.face(Role::Body).width(&item.label)).max().unwrap_or(0);
    let keys_w = items.iter().map(|item| ctx.face(Role::Caption).width(item.keys)).max().unwrap_or(0);
    let pad = ctx.px(4);
    let w = (label_w + keys_w + ctx.px(28 + 32 + 10) + pad * 2).max(ctx.px(200));
    let mut y = head.bottom() + ctx.px(2) as i32 + pad as i32;
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        // 25.11: compact rows and separators of the shared menu style.
        if index > 0 && item.group != items[index - 1].group {
            y += ctx.px(7) as i32;
        }
        rows.push(Rect::new(head.x + pad as i32, y, w - pad * 2, ctx.px(theme::MENU_ITEM_H)));
        y += ctx.px(theme::MENU_ITEM_H) as i32;
    }
    let h = (y + pad as i32 - head.bottom() - ctx.px(2) as i32).max(0) as u32;
    let x = head.x.min(area.right() - w as i32).max(area.x);
    let dx = x - head.x;
    let rect = Rect::new(x, head.bottom() + ctx.px(2) as i32, w, h);
    (rect, rows.into_iter().map(|row| row.translate(dx, 0)).collect())
}

/// Карточка вопроса «Сохранить изменения?» и три кнопки.
fn ask_plan(ctx: Ctx, area: Rect) -> (Rect, [Rect; 3]) {
    let w = ctx.px(460).min(area.w.saturating_sub(ctx.px(24)));
    let h = ctx.px(150);
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 3, w, h);
    let pad = ctx.px(20) as i32;
    let bw = ctx.px(124);
    let by = card.bottom() - pad - ctx.px(32) as i32;
    let cancel = Rect::new(card.right() - pad - bw as i32, by, bw, ctx.px(32));
    let discard = Rect::new(cancel.x - ctx.px(8) as i32 - bw as i32, by, bw, ctx.px(32));
    let save = Rect::new(discard.x - ctx.px(8) as i32 - bw as i32, by, bw, ctx.px(32));
    (card, [save, discard, cancel])
}

/// Цвет куска подсветки.
fn ink(ctx: Ctx, kind: Kind) -> mini_ui::Color {
    let p = ctx.palette;
    match kind {
        Kind::Text => p.ink,
        Kind::Keyword => p.acc_ink,
        Kind::String => p.ok_ink,
        Kind::Number => p.warn,
        Kind::Comment => p.ink4,
        Kind::Name => p.bad_ink,
    }
}

/// Подпись встроенного шрифта.
const BUILTIN_FONT: &str = "Встроенный моноширинный";

/// Где текст по горизонтали и как высока строка.
struct TextMetrics {
    /// Левый край текста.
    x: i32,
    gutter_w: u32,
    line_h: i32,
    /// Базовая линия от верха строки.
    ascent: i32,
    /// Ширина «0» — столбец для PageDown и прокрутки.
    zero_w: f32,
}

impl Editor {
    fn metrics(&self, ctx: Ctx, plan: &Plan) -> TextMetrics {
        let mono = ctx.face(Role::Mono);
        let digit_w = mono.width("0").max(1);
        let gutter_w = if self.numbers { digit_w * (GUTTER_CHARS as u32 + 1) + ctx.px(12) } else { 0 };
        let x = plan.text.x + gutter_w as i32 + ctx.px(10) as i32;
        match &self.font.face {
            Some(face) => {
                let mut face = face.borrow_mut();
                face.set_px(ctx.px(self.font.size) as f32);
                let metrics = face.font.line_metrics(face.px);
                let zero_w = face.font.advance('0', face.px).max(1.0);
                TextMetrics { x, gutter_w, line_h: metrics.line as i32 + 1 + ctx.px(2) as i32, ascent: (metrics.ascent + 0.5) as i32, zero_w }
            }
            None => TextMetrics { x, gutter_w, line_h: i32::from(mono.line) + ctx.px(2) as i32, ascent: 0, zero_w: digit_w as f32 },
        }
    }

    /// Шаг пера знака.
    fn advance(&self, metrics: &TextMetrics, ch: char) -> f32 {
        match &self.font.face {
            Some(face) => {
                let face = face.borrow();
                let ch = if ch == '\t' { ' ' } else { ch };
                face.font.advance(ch, face.px)
            }
            None => metrics.zero_w,
        }
    }

    /// Нарисовать кусок строки с `x` (верх строки — `y`); ответ — `x` после него.
    #[allow(clippy::too_many_arguments)]
    fn draw_piece(&self, s: &mut Surface, ctx: Ctx, metrics: &TextMetrics, x: f32, y: i32, text: &str, color: mini_ui::Color, clip: Rect) -> f32 {
        match &self.font.face {
            None => {
                let count = text.chars().count() as f32;
                let visible: String = text.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
                let width = ((clip.right() as f32 - x).max(0.0) as u32).min((count * metrics.zero_w) as u32 + 1);
                paint::text_clipped(ctx, s, Role::Mono, x as i32, y, width, &visible, color);
                x + count * metrics.zero_w
            }
            Some(face) => {
                let mut face = face.borrow_mut();
                let mut pen = x;
                for ch in text.chars() {
                    let ch = if ch == '\t' { ' ' } else { ch };
                    let glyph = face.glyph(ch);
                    if pen >= clip.right() as f32 {
                        break;
                    }
                    blit_glyph(s, glyph, pen as i32 + glyph.left, y + metrics.ascent + glyph.top, color, clip);
                    pen += glyph.advance;
                }
                pen
            }
        }
    }

    /// Куски строки по экранным строкам: без переноса — один от `left`; с
    /// переносом — по ширине, разрыв по последнему пробелу.
    fn segments(&self, metrics: &TextMetrics, line: &str, width: f32) -> Vec<(usize, usize)> {
        let len = line.chars().count();
        if !self.wrap {
            return vec![(self.left.min(len), len)];
        }
        let mut out = Vec::new();
        let mut start = 0;
        let mut pen = 0.0f32;
        let mut space = None;
        for (col, ch) in line.chars().enumerate() {
            let step = self.advance(metrics, ch);
            if pen + step > width && col > start {
                let cut = match space {
                    Some(at) if at > start => at + 1,
                    _ => col,
                };
                out.push((start, cut));
                pen = line.chars().skip(cut).take(col - cut).map(|c| self.advance(metrics, c)).sum();
                start = cut;
                space = None;
            }
            if ch == ' ' {
                space = Some(col);
            }
            pen += step;
        }
        out.push((start, len));
        out
    }

    /// Столбец под точкой `x` в куске строки `row`, начинающемся с `from`.
    fn col_at(&self, metrics: &TextMetrics, row: usize, from: usize, to: usize, x: i32) -> usize {
        let Some(line) = self.lines.get(row) else {
            return 0;
        };
        let mut pen = metrics.x as f32;
        for (col, ch) in line.chars().enumerate().skip(from).take(to.saturating_sub(from)) {
            let step = self.advance(metrics, ch);
            if (x as f32) < pen + step / 2.0 {
                return col;
            }
            pen += step;
        }
        to
    }

    /// Место в тексте под точкой окна — по раскладке последней отрисовки.
    /// Выше текста — первая показанная строка, ниже — последняя.
    fn pos_at(&self, ctx: Ctx, plan: &Plan, x: i32, y: i32) -> Pos {
        let metrics = self.metrics(ctx, plan);
        let hit = {
            let map = self.screen_map.borrow();
            map.iter().rev().find(|(_, _, _, top)| y >= *top).or(map.first()).copied()
        };
        match hit {
            Some((row, from, to, _)) => (row, self.col_at(&metrics, row, from, to, x)),
            None => (self.top.min(self.lines.len() - 1), 0),
        }
    }

    /// Слово под местом (двойной щелчок): буквы и цифры, пробелы подряд или
    /// один знак препинания.
    fn word_at(&self, pos: Pos) -> (Pos, Pos) {
        let chars: Vec<char> = self.lines[pos.0].chars().collect();
        if chars.is_empty() {
            return (pos, pos);
        }
        let class = |ch: char| if ch.is_alphanumeric() || ch == '_' { 1 } else if ch.is_whitespace() { 0 } else { 2 };
        let at = pos.1.min(chars.len() - 1);
        let kind = class(chars[at]);
        if kind == 2 {
            return ((pos.0, at), (pos.0, at + 1));
        }
        let mut from = at;
        while from > 0 && class(chars[from - 1]) == kind {
            from -= 1;
        }
        let mut to = at + 1;
        while to < chars.len() && class(chars[to]) == kind {
            to += 1;
        }
        ((pos.0, from), (pos.0, to))
    }

    /// Строка целиком (тройной щелчок) — с переводом строки, если он есть.
    fn line_span(&self, row: usize) -> (Pos, Pos) {
        if row + 1 < self.lines.len() { ((row, 0), (row + 1, 0)) } else { ((row, 0), (row, self.line_len(row))) }
    }

    /// Тянуть выделение мышью до `pos`: знаками — каретка за указателем;
    /// словами и строками — выделено всё от первого слова (строки) до
    /// текущего, каретка — на подвижном конце.
    fn extend_to(&mut self, pos: Pos) {
        let Some(press) = self.press else {
            return;
        };
        let span = match press.unit {
            Unit::Char => {
                self.row = pos.0.min(self.lines.len() - 1);
                self.col = pos.1.min(self.line_len(self.row));
                return;
            }
            Unit::Word => self.word_at(pos),
            Unit::Line => self.line_span(pos.0),
        };
        let (origin_start, origin_end) = press.origin;
        if span.0 < origin_start {
            self.anchor = Some(origin_end);
            (self.row, self.col) = span.0;
        } else {
            self.anchor = Some(origin_start);
            (self.row, self.col) = span.1.max(origin_end);
        }
    }

    /// Щелчок левой кнопкой в тексте: каретка, Shift — тянуть выделение до
    /// точки, двойной — слово, тройной — строка; кнопка зажата — дальше
    /// выделяет движение ([`App::drag`]).
    fn press_text(&mut self, area: Rect, ctx: Ctx, plan: &Plan, x: i32, y: i32) {
        let pos = self.pos_at(ctx, plan, x, y);
        let now = user_progs::monotonic_ms();
        let shift = self.click_mods & WIN_MOD_SHIFT != 0;
        let (when, lx, ly, count) = self.last_click;
        let near = (x - lx).unsigned_abs() <= ctx.px(MULTI_CLICK_PX) && (y - ly).unsigned_abs() <= ctx.px(MULTI_CLICK_PX);
        let count = if !shift && near && now.saturating_sub(when) <= MULTI_CLICK_MS { count % 3 + 1 } else { 1 };
        self.last_click = (now, x, y, count);
        self.last_edit = EditKind::None;
        let (unit, origin) = match count {
            2 => (Unit::Word, self.word_at(pos)),
            3 => (Unit::Line, self.line_span(pos.0)),
            _ if shift => {
                // Shift+щелчок: второй конец — прежняя каретка (или начало
                // прежнего выделения).
                let anchor = self.anchor.unwrap_or((self.row, self.col));
                self.anchor = Some(anchor);
                (Unit::Char, (anchor, anchor))
            }
            _ => {
                self.anchor = Some(pos);
                (Unit::Char, (pos, pos))
            }
        };
        if unit != Unit::Char {
            self.anchor = Some(origin.0);
        }
        (self.row, self.col) = if unit == Unit::Char { pos } else { origin.1 };
        self.press = Some(Press { unit, origin, area, ctx, x, y });
        if count > 1 {
            println(&format!("notepad: {} selected by a {}-click", if count == 2 { "word" } else { "line" }, count));
        }
    }

    /// Кнопка зажата, а указатель над или под текстом: прокрутить на строку
    /// и тянуть выделение (зовётся с каждым витком цикла, ~30 мс).
    fn drag_scroll(&mut self) -> bool {
        let Some(press) = self.press else {
            return false;
        };
        let plan = Plan::new(press.ctx, press.area);
        let top_y = plan.text.y + self.find_h(press.ctx) as i32;
        let rows = self.page_rows.get().max(1);
        let row = if press.y < top_y && self.top > 0 {
            self.top -= 1;
            self.top
        } else if press.y >= plan.text.bottom() && self.top + rows < self.lines.len() {
            self.top += 1;
            (self.top + rows - 1).min(self.lines.len() - 1)
        } else {
            return false;
        };
        let metrics = self.metrics(press.ctx, &plan);
        let from = if self.wrap { 0 } else { self.left };
        let col = self.col_at(&metrics, row, from, self.line_len(row), press.x);
        self.extend_to((row, col));
        true
    }

    fn draw_text(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let metrics = self.metrics(ctx, plan);
        let gutter = Rect::new(plan.text.x, plan.text.y, metrics.gutter_w, plan.text.h);
        // Классика (22.11): поле текста белое и вдавленное, как у Блокнота
        // Windows 98.
        let classic = theme::is_classic();
        if classic {
            s.fill(plan.text, ctx.flat(p.sunk));
        }
        if self.numbers {
            s.fill(gutter, ctx.flat(p.panel));
            draw::vline(s, gutter.right() - 1, gutter.y, gutter.h, p.line.color, p.line.alpha);
        }
        if classic {
            mini_ui::classic::sunken(s, plan.text, mini_ui::classic::unit(ctx.scale));
        }
        let right = plan.text.right() - ctx.px(10) as i32;
        let clip = Rect::new(metrics.x, plan.text.y, (right - metrics.x).max(0) as u32, plan.text.h);
        let top_y = plan.text.y + ctx.px(6) as i32 + self.find_h(ctx) as i32;
        let rows = ((plan.text.bottom() - top_y - ctx.px(6) as i32) / metrics.line_h).max(1) as usize;
        let cols = (((right - metrics.x).max(0) as f32) / metrics.zero_w).max(1.0) as usize;
        self.page_rows.set(rows);
        self.page_cols.set(cols);
        let selection = self.selection();
        let busy = self.dialog.is_some() || self.ask_save.is_some() || self.font_dialog.is_some();
        let width = (right - metrics.x).max(1) as f32;
        let mut map = Vec::new();
        let mut drawn = 0usize;
        let mut index = self.top;
        while drawn < rows {
            let Some(line) = self.lines.get(index) else {
                break;
            };
            let segments = self.segments(&metrics, line, width);
            let last = segments.len().saturating_sub(1);
            let len = line.chars().count();
            for (number, (from_col, to_col)) in segments.iter().copied().enumerate() {
                if drawn >= rows {
                    break;
                }
                let y = top_y + drawn as i32 * metrics.line_h;
                map.push((index, from_col, to_col, y));
                drawn += 1;
                // Где начинается каждый показанный знак куска (и его конец).
                let mut pos: Vec<f32> = Vec::new();
                let mut pen = metrics.x as f32;
                for ch in line.chars().skip(from_col).take(to_col - from_col) {
                    pos.push(pen);
                    if pen > right as f32 {
                        break;
                    }
                    pen += self.advance(&metrics, ch);
                }
                pos.push(pen);
                let x_at = |col: usize| -> f32 {
                    let at = col.saturating_sub(from_col);
                    match pos.get(at) {
                        Some(x) => *x,
                        None => pos.last().copied().unwrap_or(metrics.x as f32) + (at + 1 - pos.len()) as f32 * metrics.zero_w,
                    }
                };
                if index == self.row && selection.is_none() {
                    draw::blend_rect(s, Rect::new(gutter.right(), y - 1, (plan.text.right() - gutter.right()).max(0) as u32, metrics.line_h as u32), p.hover1.color, p.hover1.alpha);
                }
                // Выделенное — подложкой акцента; перевод строки — ширина «0».
                if let Some((start, end)) = selection {
                    if index >= start.0 && index <= end.0 {
                        let from = if index == start.0 { start.1 } else { 0 };
                        let to = if index == end.0 { end.1 } else { len };
                        let (a, b) = (from.max(from_col), to.min(to_col));
                        let mut x1 = if b >= a { x_at(b) } else { 0.0 };
                        let x0 = x_at(a);
                        if index != end.0 && number == last {
                            x1 += metrics.zero_w;
                        }
                        let x1 = x1.min(right as f32);
                        if b >= a && x1 > x0 {
                            draw::blend_rect(s, Rect::new(x0 as i32, y - 1, (x1 - x0) as u32, metrics.line_h as u32), p.acc, 90);
                        }
                    }
                }
                if self.numbers && number == 0 {
                    let label = format!("{}", index + 1);
                    paint::text_right(ctx, s, Role::Mono, gutter.right() - ctx.px(8) as i32, y, &label, if index == self.row { p.ink2 } else { p.ink5 });
                }
                // По кускам подсветки — каждый своим цветом.
                let mut state = self.state_at(index);
                for (from, to, kind) in syntax::spans(self.lang, line, &mut state) {
                    let (from, to) = (from.max(from_col), to.min(to_col));
                    if from >= to {
                        continue;
                    }
                    let piece: String = line.chars().skip(from).take(to - from).collect();
                    self.draw_piece(s, ctx, &metrics, x_at(from), y, &piece, ink(ctx, kind), clip);
                }
                let here = index == self.row && self.col >= from_col && (self.col < to_col || (self.col == to_col && number == last));
                if here && !busy {
                    let cx = x_at(self.col) as i32;
                    if cx <= right {
                        s.fill(Rect::new(cx, y, ctx.px(2), (metrics.line_h - ctx.px(2) as i32).max(1) as u32), p.acc);
                    }
                }
            }
            index += 1;
        }
        *self.screen_map.borrow_mut() = map;
        self.draw_find(s, ctx, plan);
    }

    fn draw_menu_bar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar = kit::toolbar(ctx, s, plan.bar);
        for (index, ((menu, label), head)) in MENUS.iter().zip(plan.heads).enumerate() {
            // 25.11: the open menu's title is lit, and so is the title under
            // the pointer (the tokens are translucent — drawn with their
            // alpha; the opaque white plate hid the dark-theme title).
            if self.menu.is_some_and(|(open, _)| open == *menu) {
                if theme::is_classic() {
                    mini_ui::classic::sunken(s, head, mini_ui::classic::unit(ctx.scale));
                } else {
                    draw::rounded(s, head, ctx.px(4), p.hover2.color, p.hover2.alpha);
                }
            } else if self.hot_head == Some(index) {
                if theme::is_classic() {
                    mini_ui::classic::bump(s, head, mini_ui::classic::unit(ctx.scale));
                } else {
                    draw::rounded(s, head, ctx.px(4), p.hover1.color, p.hover1.alpha);
                }
            }
            paint::text(bar, s, Role::Body, head.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, head), label, p.ink);
        }
        let title = self.title();
        paint::text_right(bar, s, Role::Strong, plan.bar.right() - ctx.px(14) as i32, paint::baseline(ctx, Role::Strong, plan.bar), &title, if self.dirty { p.acc_ink } else { p.ink2 });
    }

    fn draw_dropdown(&self, s: &mut Surface, ctx: Ctx, area: Rect, plan: &Plan) {
        let Some((menu, selected)) = self.menu else {
            return;
        };
        let Some(head) = MENUS.iter().position(|(m, _)| *m == menu).map(|index| plan.heads[index]) else {
            return;
        };
        let items = self.items(menu);
        let (rect, rows) = dropdown(ctx, area, head, &items);
        let inner = kit::card(ctx, s, rect);
        let p = inner.palette;
        for (index, (item, row)) in items.iter().zip(&rows).enumerate() {
            if index > 0 && item.group != items[index - 1].group {
                let line_y = row.y - ctx.px(4) as i32;
                if theme::is_classic() {
                    mini_ui::classic::groove_h(s, rect.x + ctx.px(3) as i32, line_y, rect.w.saturating_sub(ctx.px(6)), mini_ui::classic::unit(ctx.scale));
                } else {
                    paint::separator(inner, s, row.x + ctx.px(8) as i32, line_y, row.w.saturating_sub(ctx.px(16)));
                }
            }
            // 25.11: the shared menu plate (pointer or arrows), as in every menu.
            let on = index == selected;
            if on {
                kit::menu_highlight(inner, s, *row, true);
            }
            let ink = if on { p.ink } else { p.ink2 };
            paint::text(inner, s, Role::Body, row.x + ctx.px(12) as i32, paint::baseline(ctx, Role::Body, *row), &item.label, ink);
            if !item.keys.is_empty() {
                let keys_ink = if on && theme::is_classic() { mini_ui::Color::rgb(0xFF, 0xFF, 0xFF) } else { p.ink4 };
                paint::text_right(inner, s, Role::Caption, row.right() - ctx.px(10) as i32, paint::baseline(ctx, Role::Caption, *row), item.keys, keys_ink);
            }
        }
    }

    fn draw_ask(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        if self.ask_save.is_none() {
            return;
        }
        let p = ctx.palette;
        draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 90);
        let (card, [save, discard, cancel]) = ask_plan(ctx, area);
        let inner = kit::card(ctx, s, card);
        let pad = ctx.px(20) as i32;
        paint::text(inner, s, Role::Title, card.x + pad, card.y + pad, "Блокнот", p.ink);
        let question = format!("Сохранить изменения в «{}»?", self.name());
        paint::text_clipped(inner, s, Role::Body, card.x + pad, card.y + pad + ctx.px(32) as i32, card.w - ctx.px(40), &question, p.ink2);
        paint::button(inner, s, save, Weight::Primary, "Сохранить", false);
        paint::button(inner, s, discard, Weight::Normal, "Не сохранять", false);
        paint::button(inner, s, cancel, Weight::Normal, "Отмена", false);
    }

    /// Ответ на «Сохранить изменения?»: 0 — сохранить, 1 — не сохранять, 2 — отмена.
    fn answer_ask(&mut self, choice: u8) {
        let Some(then) = self.ask_save.take() else {
            return;
        };
        match choice {
            0 => {
                if self.save() {
                    self.proceed(then);
                }
            }
            1 => {
                self.dirty = false;
                self.proceed(then);
            }
            _ => {}
        }
    }
}

impl App for Editor {
    /// Smallest window (24.11, WM SDK): the menu row, a few lines of text and
    /// the status line with the syntax switch.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(480, 260)
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        self.draw_menu_bar(s, ctx, &plan);
        self.draw_text(s, ctx, &plan);

        // ── Строка состояния ────────────────────────────────────────────────
        let left = match &self.note {
            Some(note) => note.clone(),
            None => String::from("Ctrl+S — сохранить    Ctrl+O — открыть    Ctrl+Z — отменить"),
        };
        let right = format!("Строка {}, столбец {}  ·  UTF-8  ·  строк {}", self.row + 1, self.col + 1, self.lines.len());
        // 24.11: a narrow window keeps the hint left of the position, and
        // drops the position when even it does not fit (they overlapped).
        let right_x = plan.lang.x - ctx.px(14) as i32 - ctx.face(Role::Caption).width(&right) as i32;
        let show_right = right_x > plan.status.x + ctx.px(120) as i32;
        let left_end = if show_right { right_x - ctx.px(12) as i32 } else { plan.lang.x };
        kit::status_bar(ctx, s, plan.status, "");
        kit::status_bar(ctx, s, Rect::new(plan.status.x, plan.status.y, (left_end - plan.status.x).max(0) as u32, plan.status.h), &left);
        if show_right {
            paint::text_right(ctx, s, Role::Caption, plan.lang.x - ctx.px(14) as i32, paint::baseline(ctx, Role::Caption, plan.status), &right, p.ink4);
        }
        draw::vline(s, plan.lang.x, plan.lang.y + ctx.px(6) as i32, plan.lang.h - ctx.px(12), p.line.color, p.line.alpha);
        if self.lang_menu.is_some() {
            draw::blend_rect(s, plan.lang, p.hover1.color, p.hover1.alpha);
        }
        let g = ctx.px(12);
        iconset::draw(s, Fi::ChevronDown, plan.lang.right() - (g + ctx.px(12)) as i32, plan.lang.y + (plan.lang.h - g) as i32 / 2, g, p.ink3, 255);
        paint::text_clipped(ctx, s, Role::Caption, plan.lang.x + ctx.px(12) as i32, paint::baseline(ctx, Role::Caption, plan.lang), plan.lang.w - ctx.px(36), self.lang.name(), p.ink2);
        if let Some(selected) = self.lang_menu {
            let (rect, entries) = plan.lang_menu(ctx, area);
            kit::draw_menu(ctx, s, rect, &entries, selected);
        }
        self.draw_dropdown(s, ctx, area, &plan);
        self.draw_font_dialog(s, ctx, area);
        self.draw_ask(s, ctx, area);
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        if self.font_dialog.is_some() {
            self.font_key(code);
            return true;
        }
        // Окно выбора файла открыто: стол ввода нам не даёт; что дошло — мимо.
        if self.dialog.is_some() {
            return false;
        }
        if self.ask_save.is_some() {
            match code {
                ENTER => self.answer_ask(0),
                ESCAPE => self.answer_ask(2),
                _ => match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                    Some('s') | Some('y') => self.answer_ask(0),
                    Some('n') => self.answer_ask(1),
                    _ => {}
                },
            }
            return true;
        }
        if self.find.is_some() && self.menu.is_none() && self.lang_menu.is_none() && self.find_key(code, mods, latin) {
            return true;
        }
        if let Some((menu, selected)) = self.menu {
            let count = self.items(menu).len();
            let index = MENUS.iter().position(|(m, _)| *m == menu).unwrap_or(0);
            match code {
                WIN_KEY_UP => self.menu = Some((menu, if selected >= count { count.saturating_sub(1) } else { selected.saturating_sub(1) })),
                WIN_KEY_DOWN => self.menu = Some((menu, if selected >= count { 0 } else { (selected + 1).min(count.saturating_sub(1)) })),
                WIN_KEY_LEFT => self.menu = Some((MENUS[(index + MENUS.len() - 1) % MENUS.len()].0, 0)),
                WIN_KEY_RIGHT => self.menu = Some((MENUS[(index + 1) % MENUS.len()].0, 0)),
                ENTER => {
                    if let Some(item) = self.items(menu).get(selected) {
                        let cmd = item.cmd;
                        self.run(cmd);
                    }
                }
                _ => self.menu = None,
            }
            return true;
        }
        if let Some(selected) = self.lang_menu {
            match code {
                WIN_KEY_UP => self.lang_menu = Some(selected.saturating_sub(1)),
                WIN_KEY_DOWN => self.lang_menu = Some((selected + 1).min(Lang::ALL.len() - 1)),
                ENTER => {
                    self.lang_menu = None;
                    self.set_lang(Lang::ALL[selected]);
                }
                _ => self.lang_menu = None,
            }
            return true;
        }
        if code == F3 {
            self.run(Cmd::FindNext);
            return true;
        }
        // F10 — полоса меню, как в Windows.
        if code == F10 {
            self.menu = Some((Menu::File, 0));
            return true;
        }
        self.note = None;
        let ctrl = mods & WIN_MOD_CTRL != 0;
        let shift = mods & WIN_MOD_SHIFT != 0;
        // Сочетания — по латинской букве клавиши: Ctrl+S в русской раскладке
        // тоже Ctrl+S.
        if ctrl {
            let cmd = match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                Some('n') => Some(Cmd::New),
                Some('o') => Some(Cmd::Open),
                Some('s') if shift => Some(Cmd::SaveAs),
                Some('s') => Some(Cmd::Save),
                Some('z') => Some(Cmd::Undo),
                Some('y') => Some(Cmd::Redo),
                Some('x') => Some(Cmd::Cut),
                Some('c') => Some(Cmd::Copy),
                Some('v') => Some(Cmd::Paste),
                Some('a') => Some(Cmd::SelectAll),
                Some('f') => Some(Cmd::Find),
                Some('h') => Some(Cmd::Replace),
                _ => None,
            };
            if let Some(cmd) = cmd {
                self.run(cmd);
                return true;
            }
        }
        let page = self.page_rows.get().max(1);
        match code {
            // Прежние коды Ctrl+буква — на случай клавиатуры без латинской
            // буквы в событии.
            CTRL_S if latin == 0 => {
                self.save();
            }
            CTRL_O if latin == 0 => self.guarded(Then::Open),
            CTRL_N if latin == 0 => self.guarded(Then::New),
            ENTER => self.newline(),
            BACKSPACE => self.backspace(),
            WIN_KEY_DELETE => self.delete(),
            WIN_KEY_LEFT => {
                // Без Shift стрелка с выделением ставит каретку на его край.
                if let (false, Some((start, _))) = (shift, self.selection()) {
                    self.move_caret(start.0, start.1, false);
                } else if self.col > 0 {
                    self.move_caret(self.row, self.col - 1, shift);
                } else if self.row > 0 {
                    let row = self.row - 1;
                    self.move_caret(row, self.line_len(row), shift);
                } else {
                    self.move_caret(self.row, self.col, shift);
                }
            }
            WIN_KEY_RIGHT => {
                if let (false, Some((_, end))) = (shift, self.selection()) {
                    self.move_caret(end.0, end.1, false);
                } else if self.col < self.line_len(self.row) {
                    self.move_caret(self.row, self.col + 1, shift);
                } else if self.row + 1 < self.lines.len() {
                    self.move_caret(self.row + 1, 0, shift);
                } else {
                    self.move_caret(self.row, self.col, shift);
                }
            }
            WIN_KEY_UP => self.move_caret(self.row.saturating_sub(1), self.col, shift),
            WIN_KEY_DOWN => self.move_caret(self.row + 1, self.col, shift),
            WIN_KEY_PAGE_UP => self.move_caret(self.row.saturating_sub(page), self.col, shift),
            WIN_KEY_PAGE_DOWN => self.move_caret(self.row + page, self.col, shift),
            WIN_KEY_HOME => {
                // Первое нажатие — к началу текста строки, второе — к нулю.
                let indent = self.lines[self.row].chars().take_while(|c| c.is_whitespace()).count();
                let col = if self.col == indent { 0 } else { indent };
                self.move_caret(self.row, col, shift);
            }
            WIN_KEY_END => self.move_caret(self.row, self.line_len(self.row), shift),
            TAB => {
                for _ in 0..4 {
                    self.insert(' ');
                }
            }
            code if code < 0x0100_0000 && !ctrl => match char::from_u32(code).filter(|ch| !ch.is_control()) {
                Some(ch) => self.insert(ch),
                None => return false,
            },
            _ => return false,
        }
        self.follow();
        true
    }

    fn click_with(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32, mods: i32) -> bool {
        self.click_mods = mods;
        let handled = self.click(area, ctx, x, y);
        self.click_mods = 0;
        handled
    }

    /// Мышь тянет выделение (24.11).
    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let Some(press) = self.press.as_mut() else {
            return false;
        };
        (press.area, press.ctx, press.x, press.y) = (area, ctx, x, y);
        let plan = Plan::new(ctx, area);
        let pos = self.pos_at(ctx, &plan, x, y);
        let before = (self.anchor, self.row, self.col);
        self.extend_to(pos);
        self.follow();
        before != (self.anchor, self.row, self.col)
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        if self.press.take().is_none() {
            return false;
        }
        if let Some(text) = self.selected_text() {
            println(&format!("notepad: {} char(s) selected by the mouse", text.chars().count()));
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if self.font_dialog.is_some() {
            self.font_click(ctx, area, x, y);
            return true;
        }
        if self.dialog.is_some() {
            return false;
        }
        if self.ask_save.is_some() {
            let (_, buttons) = ask_plan(ctx, area);
            if let Some(index) = buttons.iter().position(|button| button.contains(x, y)) {
                self.answer_ask(index as u8);
            }
            return true;
        }
        if let Some((menu, _)) = self.menu {
            let head = MENUS.iter().position(|(m, _)| *m == menu).map(|index| plan.heads[index]).unwrap_or(Rect::EMPTY);
            let items = self.items(menu);
            let (_, rows) = dropdown(ctx, area, head, &items);
            if let Some(index) = rows.iter().position(|row| row.contains(x, y)) {
                let cmd = items[index].cmd;
                self.run(cmd);
                return true;
            }
            self.menu = None;
            // Щелчок по другому заголовку — сразу его меню.
            if let Some(index) = plan.heads.iter().position(|head| head.contains(x, y)) {
                if MENUS[index].0 != menu {
                    self.menu = Some((MENUS[index].0, 0));
                }
            }
            return true;
        }
        if let Some(index) = plan.heads.iter().position(|head| head.contains(x, y)) {
            self.menu = Some((MENUS[index].0, 0));
            self.lang_menu = None;
            return true;
        }
        if self.lang_menu.is_some() {
            let (rect, entries) = plan.lang_menu(ctx, area);
            if let Some(index) = kit::menu_hit(ctx, rect, &entries, x, y) {
                self.set_lang(Lang::ALL[index]);
            }
            self.lang_menu = None;
            return true;
        }
        if plan.lang.contains(x, y) {
            self.lang_menu = Some(Lang::ALL.iter().position(|lang| *lang == self.lang).unwrap_or(0));
            return true;
        }
        if self.find.is_some() && y < plan.text.y + self.find_h(ctx) as i32 && plan.text.contains(x, y) {
            return self.find_click(ctx, &plan, x);
        }
        if plan.text.contains(x, y) {
            // Экранная строка под точкой — из раскладки последней отрисовки.
            self.press_text(area, ctx, &plan, x, y);
            return true;
        }
        false
    }

    /// 25.11: hover on the menu bar and in the menus — the title under the
    /// pointer is lit; with a menu open, moving to another title opens that
    /// menu (as in Windows) and the row under the pointer is lit.
    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.font_dialog.is_some() || self.dialog.is_some() || self.ask_save.is_some() {
            return false;
        }
        let plan = Plan::new(ctx, area);
        let before = (self.hot_head, self.menu, self.lang_menu);
        let head_at = plan.heads.iter().position(|head| head.contains(x, y));
        self.hot_head = head_at;
        if let Some((menu, _)) = self.menu {
            if let Some(index) = head_at.filter(|index| MENUS[*index].0 != menu) {
                self.menu = Some((MENUS[index].0, usize::MAX));
            } else if head_at.is_none() {
                let head = MENUS.iter().position(|(m, _)| *m == menu).map(|index| plan.heads[index]).unwrap_or(Rect::EMPTY);
                let items = self.items(menu);
                let (_, rows) = dropdown(ctx, area, head, &items);
                let lit = rows.iter().position(|row| row.contains(x, y)).unwrap_or(usize::MAX);
                self.menu = Some((menu, lit));
            }
        } else if let Some(selected) = self.lang_menu {
            let (rect, entries) = plan.lang_menu(ctx, area);
            if let Some(index) = kit::menu_hit(ctx, rect, &entries, x, y) {
                if index != selected {
                    self.lang_menu = Some(index);
                }
            }
        }
        before != (self.hot_head, self.menu, self.lang_menu)
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        if self.dialog.is_some() {
            return false;
        }
        let rows = self.page_rows.get();
        let last = self.lines.len().saturating_sub(rows.max(1));
        self.top = if clicks > 0 { self.top.saturating_sub(3) } else { (self.top + 3).min(last) };
        true
    }

    /// Имя окна: «Notepad» — для журнала и стенда, после `\x1f` — то, что
    /// видит человек в заголовке и на панели задач.
    fn title(&self) -> Option<String> {
        Some(format!("Notepad\x1f{} — Блокнот", Editor::title(self)))
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        false
    }

    /// Ответ окна выбора файла (`/bin/filedlg`) — не дожидаясь его.
    fn step(&mut self) -> bool {
        if self.drag_scroll() {
            return true;
        }
        let Some(outcome) = self.dialog.as_mut().and_then(Dialog::poll) else {
            return false;
        };
        self.dialog_done(outcome);
        true
    }

    /// `q` — буква текста, а не выход: блокнот закрывают крестиком или Ctrl+W.
    fn quits_on_q(&self) -> bool {
        false
    }

    /// Несохранённое — сначала вопрос.
    fn close_requested(&mut self) -> bool {
        if self.dirty {
            self.dialog = None;
            self.menu = None;
            self.ask_save = Some(Then::Quit);
            println("notepad: asking to save before closing");
            return false;
        }
        true
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }
}

// ---------------------------------------------------------------------------
// Файлы
// ---------------------------------------------------------------------------

fn read_file(path: &str) -> Result<String, &'static str> {
    let fd = open(path);
    if fd < 0 {
        return Err(if fd == user_abi::ERR_NOT_FOUND { "нет такого файла" } else if fd == user_abi::ERR_PERMISSION { "нет прав" } else { "не открывается" });
    }
    let size = file_size(fd);
    if size < 0 || size as usize > MAX_FILE {
        close(fd);
        return Err("файл больше четырёх мегабайт");
    }
    let mut buffer = vec![0u8; size as usize];
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    buffer.truncate(got);
    String::from_utf8(buffer).map_err(|_| "это не текст UTF-8")
}

/// Содержимое папки для окна выбора: «..» (если не корень), папки, файлы.
fn list_dir(path: &str) -> Vec<(String, bool)> {
    let mut folders = Vec::new();
    let mut files = Vec::new();
    let fd = open(path);
    if fd >= 0 {
        let mut entry = Dirent::default();
        while readdir_raw(fd, &mut entry) > 0 {
            let len = (entry.name_len as usize).min(entry.name.len());
            let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
                continue;
            };
            if name == "." || name == ".." || name.starts_with('.') {
                continue;
            }
            if entry.kind == KIND_DIRECTORY {
                folders.push((name.to_string(), true));
            } else {
                files.push((name.to_string(), false));
            }
            if folders.len() + files.len() >= 2048 {
                break;
            }
        }
        close(fd);
    }
    folders.sort();
    files.sort();
    let mut out = Vec::new();
    if path != "/" {
        out.push((String::from(".."), true));
    }
    out.extend(folders);
    out.extend(files);
    out
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) | None => String::from("/"),
        Some(at) => path[..at].to_string(),
    }
}

/// Положить растр глифа цветом `color` по его покрытию, не выходя за `clip`.
fn blit_glyph(s: &mut Surface, glyph: &ttf::Bitmap, x: i32, y: i32, color: mini_ui::Color, clip: Rect) {
    let w = glyph.width as i32;
    for (row, line) in glyph.coverage.chunks(w.max(1) as usize).enumerate() {
        let py = y + row as i32;
        if py < clip.y || py >= clip.bottom() || py < 0 {
            continue;
        }
        for (column, coverage) in line.iter().enumerate() {
            let px = x + column as i32;
            if *coverage == 0 || px < clip.x || px >= clip.right() || px < 0 {
                continue;
            }
            let under = mini_ui::Color::from_pixel(s.get(px as u32, py as u32));
            s.put(px as u32, py as u32, under.mix(color, *coverage).pixel());
        }
    }
}

/// Прочитать файл целиком, не больше `limit` байт.
fn read_bytes(path: &str, limit: usize) -> Option<Vec<u8>> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let size = file_size(fd);
    if size <= 0 || size as usize > limit {
        close(fd);
        return None;
    }
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(size as usize).is_err() {
        close(fd);
        return None;
    }
    buffer.resize(size as usize, 0u8);
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    buffer.truncate(got);
    Some(buffer)
}

/// Подпись шрифта по имени файла.
fn font_label(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    match stem {
        "Inter" => String::from("Inter"),
        "JetBrainsMono" => String::from("JetBrains Mono"),
        "DejaVuSans" => String::from("DejaVu Sans"),
        "DejaVuSansMono" => String::from("DejaVu Sans Mono"),
        "DejaVuSerif" => String::from("DejaVu Serif"),
        "FiraCode" => String::from("Fira Code"),
        "SourceCodePro" => String::from("Source Code Pro"),
        "NotoSans" => String::from("Noto Sans"),
        "NotoSansMono" => String::from("Noto Sans Mono"),
        "IBMPlexMono-Regular" => String::from("IBM Plex Mono"),
        "Hack-Regular" => String::from("Hack"),
        other => other.to_string(),
    }
}

/// Шрифты системы: встроенный и всё `.ttf` из [`FONTS_DIR`].
fn system_fonts() -> Vec<(String, Option<String>)> {
    let mut fonts: Vec<(String, Option<String>)> = list_dir(FONTS_DIR)
        .into_iter()
        .filter(|(name, folder)| !folder && name.to_ascii_lowercase().ends_with(".ttf"))
        .map(|(name, _)| (font_label(&name), Some(join(FONTS_DIR, &name))))
        .collect();
    fonts.sort();
    fonts.insert(0, (String::from(BUILTIN_FONT), None));
    fonts
}

fn error_text(code: i64) -> &'static str {
    match code {
        user_abi::ERR_NOT_FOUND => "нет такого каталога",
        user_abi::ERR_PERMISSION => "нет прав на запись",
        user_abi::ERR_NO_SPACE => "на диске нет места",
        _ => "запись не удалась",
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения — те, что ядро передало в `_start`.
    let args = unsafe { Args::new(argc, argv) };
    // Ядро делит строку запуска по пробелам, кавычек не зная: путь
    // «/root/Desktop/Новый текстовый документ.txt» со стола приходит тремя
    // словами. До 24.11 Блокнот брал первое — открывал «/root/Desktop/Новый»
    // и сохранял туда: на столе появлялся «неизвестный файл» без расширения
    // (hw-log18). Путь — все слова через пробел, как у «Файлов».
    let words: Vec<&str> = (1..args.len()).filter_map(|index| args.get(index)).collect();
    let path = (!words.is_empty()).then(|| words.join(" "));
    let info = app::start(&SPEC);
    // Один размер на глаз на 1280×720 и 2560×1440 (20.11), а не две трети
    // экрана.
    let size = app::window_size(&info, app::size::NOTEPAD);
    app::run(&SPEC, &info, size, move |_, _| Editor::new(path.as_deref()))
}
