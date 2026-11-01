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
//! * Открыть, сохранить, сохранить как (Ctrl+O, Ctrl+S, Ctrl+Shift+S) — окном
//!   со списком папок и полем имени; создать (Ctrl+N). Несохранённое — вопрос
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
use user_abi::{Dirent, KIND_DIRECTORY, Stat, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN,
    WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, clipboard_get, clipboard_set, close, file_size, open, open_write,
    println, read, readdir_raw, stat, write,
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
/// Строка выпадающего меню и окна выбора файла.
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

/// Окно «Открыть» / «Сохранить как»: папка, её содержимое, поле имени.
struct FileDialog {
    save: bool,
    dir: String,
    /// Имена и «папка ли», папки первыми.
    entries: Vec<(String, bool)>,
    selected: usize,
    top: usize,
    name: String,
    error: Option<String>,
    /// Последний щелчок по строке: её номер — для двойного щелчка.
    last_click: Option<usize>,
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
    /// Открытое меню полосы и подсвеченный пункт.
    menu: Option<(Menu, usize)>,
    /// Открыто меню языков у строки состояния: подсвеченный пункт.
    lang_menu: Option<usize>,
    /// Состояние подсветки на начало каждой строки (см. `state_at`).
    states: core::cell::RefCell<Vec<syntax::State>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    numbers: bool,
    dialog: Option<FileDialog>,
    /// Вопрос «Сохранить изменения?» и что сделать после ответа.
    ask_save: Option<Then>,
    quit: bool,
    home: String,
    font: TextFont,
    font_dialog: Option<FontDialog>,
}

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
            lang_menu: None,
            states: core::cell::RefCell::new(Vec::new()),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::None,
            numbers: true,
            dialog: None,
            ask_save: None,
            quit: false,
            home: home_dir(),
            font: TextFont { face: None, label: String::from(BUILTIN_FONT), path: None, size: 12 },
            font_dialog: None,
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
        let dir = match self.path.as_deref() {
            Some(path) => parent_of(path),
            None => self.home.clone(),
        };
        let name = if save { self.path.as_deref().map_or_else(|| String::from("Без имени.txt"), |path| path.rsplit('/').next().unwrap_or(path).to_string()) } else { String::new() };
        let mut dialog = FileDialog { save, dir, entries: Vec::new(), selected: 0, top: 0, name, error: None, last_click: None };
        dialog.reload();
        println(&format!("notepad: {} dialog at {}", if save { "save" } else { "open" }, dialog.dir));
        self.dialog = Some(dialog);
    }

    /// Готово в окне выбора файла: войти в папку или открыть / сохранить.
    fn dialog_accept(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let name = dialog.name.trim().to_string();
        // Выбрана папка и имя не набрано (или совпадает с ней) — войти в неё.
        if let Some((entry, true)) = dialog.entries.get(dialog.selected).cloned() {
            if name.is_empty() || name == entry {
                dialog.enter(&entry);
                return;
            }
        }
        if name.is_empty() {
            dialog.error = Some(String::from("Введите имя файла"));
            return;
        }
        let path = if name.starts_with('/') { name } else { join(&dialog.dir, &name) };
        let mut info = Stat::default();
        if stat(&path, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
            dialog.dir = path;
            dialog.name.clear();
            dialog.reload();
            return;
        }
        let save = dialog.save;
        self.dialog = None;
        if save {
            self.set_lang(Lang::detect(&path));
            self.path = Some(path);
            self.save();
        } else {
            self.load(&path);
        }
    }

    fn dialog_key(&mut self, code: u32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let rows = 8usize;
        match code {
            ESCAPE => self.dialog = None,
            ENTER => self.dialog_accept(),
            BACKSPACE => {
                if dialog.name.pop().is_none() {
                    // Пустое имя — Backspace уводит в папку выше, как в Проводнике.
                    let up = parent_of(&dialog.dir);
                    if up != dialog.dir {
                        dialog.dir = up;
                        dialog.reload();
                    }
                }
                dialog.error = None;
            }
            WIN_KEY_UP | WIN_KEY_DOWN => {
                let count = dialog.entries.len();
                if count > 0 {
                    dialog.selected = if code == WIN_KEY_UP { dialog.selected.saturating_sub(1) } else { (dialog.selected + 1).min(count - 1) };
                    if dialog.selected < dialog.top {
                        dialog.top = dialog.selected;
                    } else if dialog.selected >= dialog.top + rows {
                        dialog.top = dialog.selected + 1 - rows;
                    }
                    if let Some((entry, false)) = dialog.entries.get(dialog.selected).cloned() {
                        dialog.name = entry;
                    }
                }
            }
            code if code < 0x0100_0000 => {
                if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                    if dialog.name.len() < 200 {
                        dialog.name.push(ch);
                    }
                    dialog.error = None;
                }
            }
            _ => {}
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
        if self.col < self.left {
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
            ],
            Menu::View => vec![
                item(if self.numbers { "✓ Номера строк" } else { "   Номера строк" }, "", Cmd::LineNumbers, 0),
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

impl FileDialog {
    fn reload(&mut self) {
        self.entries = list_dir(&self.dir);
        self.selected = 0;
        self.top = 0;
        self.error = None;
        self.last_click = None;
    }

    fn enter(&mut self, name: &str) {
        self.dir = if name == ".." { parent_of(&self.dir) } else { join(&self.dir, name) };
        self.name.clear();
        self.reload();
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
            .map(|lang| kit::Entry { title: lang.name(), icon: None, danger: false, group: u8::from(*lang != Lang::Plain) })
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
        if index > 0 && item.group != items[index - 1].group {
            y += ctx.px(9) as i32;
        }
        rows.push(Rect::new(head.x + pad as i32, y, w - pad * 2, ctx.px(ITEM_H)));
        y += ctx.px(ITEM_H) as i32;
    }
    let h = (y + pad as i32 - head.bottom() - ctx.px(2) as i32).max(0) as u32;
    let x = head.x.min(area.right() - w as i32).max(area.x);
    let dx = x - head.x;
    let rect = Rect::new(x, head.bottom() + ctx.px(2) as i32, w, h);
    (rect, rows.into_iter().map(|row| row.translate(dx, 0)).collect())
}

/// Окно выбора файла: карточка, список, поле имени, кнопки.
struct DialogPlan {
    card: Rect,
    list: Rect,
    field: Rect,
    ok: Rect,
    cancel: Rect,
}

fn dialog_plan(ctx: Ctx, area: Rect) -> DialogPlan {
    let w = ctx.px(560).min(area.w.saturating_sub(ctx.px(24)));
    let h = ctx.px(430).min(area.h.saturating_sub(ctx.px(24)));
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
    let pad = ctx.px(20) as i32;
    let list_y = card.y + pad + ctx.px(56) as i32;
    let bottom_h = ctx.px(34 + 14 + 32 + 20);
    let list = Rect::new(card.x + pad, list_y, w - ctx.px(40), (card.bottom() - bottom_h as i32 - list_y).max(0) as u32);
    let field = Rect::new(card.x + pad, list.bottom() + ctx.px(12) as i32, w - ctx.px(40), ctx.px(34));
    let bw = ctx.px(120);
    let by = card.bottom() - pad - ctx.px(32) as i32;
    let cancel = Rect::new(card.right() - pad - bw as i32, by, bw, ctx.px(32));
    let ok = Rect::new(cancel.x - ctx.px(8) as i32 - bw as i32, by, bw, ctx.px(32));
    DialogPlan { card, list, field, ok, cancel }
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

    /// Столбец под точкой `x` строки `row`.
    fn col_at(&self, metrics: &TextMetrics, row: usize, x: i32) -> usize {
        let Some(line) = self.lines.get(row) else {
            return 0;
        };
        let mut pen = metrics.x as f32;
        for (col, ch) in line.chars().enumerate().skip(self.left) {
            let step = self.advance(metrics, ch);
            if (x as f32) < pen + step / 2.0 {
                return col;
            }
            pen += step;
        }
        line.chars().count()
    }

    fn draw_text(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let metrics = self.metrics(ctx, plan);
        let gutter = Rect::new(plan.text.x, plan.text.y, metrics.gutter_w, plan.text.h);
        if self.numbers {
            s.fill(gutter, ctx.flat(p.panel));
            draw::vline(s, gutter.right() - 1, gutter.y, gutter.h, p.line.color, p.line.alpha);
        }
        let right = plan.text.right() - ctx.px(10) as i32;
        let clip = Rect::new(metrics.x, plan.text.y, (right - metrics.x).max(0) as u32, plan.text.h);
        let rows = ((plan.text.h as i32 - ctx.px(12) as i32) / metrics.line_h).max(1) as usize;
        let cols = (((right - metrics.x).max(0) as f32) / metrics.zero_w).max(1.0) as usize;
        self.page_rows.set(rows);
        self.page_cols.set(cols);
        let top_y = plan.text.y + ctx.px(6) as i32;
        let selection = self.selection();
        let busy = self.dialog.is_some() || self.ask_save.is_some() || self.font_dialog.is_some();
        for offset in 0..rows {
            let index = self.top + offset;
            let Some(line) = self.lines.get(index) else {
                break;
            };
            let y = top_y + offset as i32 * metrics.line_h;
            // Где начинается каждый показанный знак (и конец строки).
            let mut pos: Vec<f32> = Vec::new();
            let mut pen = metrics.x as f32;
            for ch in line.chars().skip(self.left) {
                pos.push(pen);
                if pen > right as f32 {
                    break;
                }
                pen += self.advance(&metrics, ch);
            }
            pos.push(pen);
            let x_at = |col: usize| -> f32 {
                let index = col.saturating_sub(self.left);
                match pos.get(index) {
                    Some(x) => *x,
                    None => pos.last().copied().unwrap_or(metrics.x as f32) + (index + 1 - pos.len()) as f32 * metrics.zero_w,
                }
            };
            if index == self.row && selection.is_none() {
                draw::blend_rect(s, Rect::new(gutter.right(), y - 1, (plan.text.right() - gutter.right()).max(0) as u32, metrics.line_h as u32), p.hover1.color, p.hover1.alpha);
            }
            // Выделенное — подложкой акцента под знаками; перевод строки — ширина «0».
            if let Some((start, end)) = selection {
                if index >= start.0 && index <= end.0 {
                    let from = if index == start.0 { start.1 } else { 0 };
                    let len = line.chars().count();
                    let to = if index == end.0 { end.1 } else { len };
                    let x0 = x_at(from.max(self.left));
                    let mut x1 = x_at(to.max(self.left));
                    if index != end.0 {
                        x1 += metrics.zero_w;
                    }
                    let x1 = x1.min(right as f32);
                    if x1 > x0 {
                        draw::blend_rect(s, Rect::new(x0 as i32, y - 1, (x1 - x0) as u32, metrics.line_h as u32), p.acc, 90);
                    }
                }
            }
            if self.numbers {
                let number = format!("{}", index + 1);
                paint::text_right(ctx, s, Role::Mono, gutter.right() - ctx.px(8) as i32, y, &number, if index == self.row { p.ink2 } else { p.ink5 });
            }
            // По кускам подсветки — каждый своим цветом.
            let mut state = self.state_at(index);
            for (from, to, kind) in syntax::spans(self.lang, line, &mut state) {
                let from = from.max(self.left);
                if from >= to || from >= self.left + pos.len() {
                    continue;
                }
                let piece: String = line.chars().skip(from).take(to - from).collect();
                self.draw_piece(s, ctx, &metrics, x_at(from), y, &piece, ink(ctx, kind), clip);
            }
            if index == self.row && !busy && self.col >= self.left {
                let cx = x_at(self.col) as i32;
                if cx <= right {
                    s.fill(Rect::new(cx, y, ctx.px(2), (metrics.line_h - ctx.px(2) as i32).max(1) as u32), p.acc);
                }
            }
        }
    }

    fn draw_menu_bar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar = kit::toolbar(ctx, s, plan.bar);
        for ((menu, label), head) in MENUS.iter().zip(plan.heads) {
            if self.menu.is_some_and(|(open, _)| open == *menu) {
                draw::rounded(s, head, ctx.px(4), p.hover1.color, 255);
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
                paint::separator(inner, s, row.x + ctx.px(8) as i32, row.y - ctx.px(5) as i32, row.w.saturating_sub(ctx.px(16)));
            }
            if index == selected {
                draw::rounded(s, *row, ctx.px(4), p.hover1.color, 255);
            }
            paint::text(inner, s, Role::Body, row.x + ctx.px(12) as i32, paint::baseline(ctx, Role::Body, *row), &item.label, p.ink);
            if !item.keys.is_empty() {
                paint::text_right(inner, s, Role::Caption, row.right() - ctx.px(10) as i32, paint::baseline(ctx, Role::Caption, *row), item.keys, p.ink4);
            }
        }
    }

    fn draw_dialog(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let Some(dialog) = &self.dialog else {
            return;
        };
        let p = ctx.palette;
        draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 90);
        let plan = dialog_plan(ctx, area);
        let inner = kit::card(ctx, s, plan.card);
        let pad = ctx.px(20) as i32;
        let heading = if dialog.save { "Сохранить как" } else { "Открыть файл" };
        paint::text(inner, s, Role::Title, plan.card.x + pad, plan.card.y + pad, heading, p.ink);
        paint::text_clipped(inner, s, Role::Caption, plan.card.x + pad, plan.card.y + pad + ctx.px(28) as i32, plan.card.w - ctx.px(40), &dialog.dir, p.ink3);
        paint::sunk(inner, s, plan.list, ctx.px(6));
        let rows = (plan.list.h / ctx.px(ITEM_H)).max(1) as usize;
        for (offset, (name, folder)) in dialog.entries.iter().skip(dialog.top).take(rows).enumerate() {
            let index = dialog.top + offset;
            let row = Rect::new(plan.list.x + ctx.px(4) as i32, plan.list.y + ctx.px(2) as i32 + (offset as u32 * ctx.px(ITEM_H)) as i32, plan.list.w - ctx.px(8), ctx.px(ITEM_H));
            if index == dialog.selected {
                draw::rounded(s, row, ctx.px(4), p.acc, 60);
            }
            let g = ctx.px(16);
            let icon = if *folder { Fi::Folder } else { Fi::File };
            iconset::draw(s, icon, row.x + ctx.px(8) as i32, row.y + (row.h - g) as i32 / 2, g, if *folder { p.warn } else { p.ink3 }, 255);
            paint::text_clipped(inner, s, Role::Body, row.x + ctx.px(32) as i32, paint::baseline(ctx, Role::Body, row), row.w - ctx.px(40), name, p.ink);
        }
        paint::sunk(inner, s, plan.field, ctx.px(6));
        draw::rounded_stroke(s, plan.field, ctx.px(6), p.acc, 255);
        let tx = plan.field.x + ctx.px(10) as i32;
        paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(ctx, Role::Body, plan.field), plan.field.w - ctx.px(20), &dialog.name, p.ink);
        let tw = ctx.face(Role::Body).width(&dialog.name).min(plan.field.w - ctx.px(20));
        s.fill(Rect::new(tx + tw as i32 + 1, plan.field.y + ctx.px(8) as i32, ctx.px(2), plan.field.h - ctx.px(16)), p.acc);
        let hint = dialog.error.clone().unwrap_or_else(|| String::from("Enter — готово · Backspace — папка выше · Esc — отмена"));
        let hint_ink = if dialog.error.is_some() { p.bad_ink } else { p.ink4 };
        paint::text_clipped(inner, s, Role::Caption, plan.card.x + pad, plan.ok.y + ctx.px(8) as i32, (plan.ok.x - plan.card.x - pad - ctx.px(8) as i32).max(0) as u32, &hint, hint_ink);
        paint::button(inner, s, plan.ok, Weight::Primary, if dialog.save { "Сохранить" } else { "Открыть" }, false);
        paint::button(inner, s, plan.cancel, Weight::Normal, "Отмена", false);
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
        kit::status_bar(ctx, s, plan.status, &left);
        let right = format!("Строка {}, столбец {}  ·  UTF-8  ·  строк {}", self.row + 1, self.col + 1, self.lines.len());
        paint::text_right(ctx, s, Role::Caption, plan.lang.x - ctx.px(14) as i32, paint::baseline(ctx, Role::Caption, plan.status), &right, p.ink4);
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
        self.draw_dialog(s, ctx, area);
        self.draw_font_dialog(s, ctx, area);
        self.draw_ask(s, ctx, area);
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        if self.font_dialog.is_some() {
            self.font_key(code);
            return true;
        }
        if self.dialog.is_some() {
            self.dialog_key(code);
            return true;
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
        if let Some((menu, selected)) = self.menu {
            let count = self.items(menu).len();
            let index = MENUS.iter().position(|(m, _)| *m == menu).unwrap_or(0);
            match code {
                WIN_KEY_UP => self.menu = Some((menu, selected.saturating_sub(1))),
                WIN_KEY_DOWN => self.menu = Some((menu, (selected + 1).min(count.saturating_sub(1)))),
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

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if self.font_dialog.is_some() {
            self.font_click(ctx, area, x, y);
            return true;
        }
        if self.dialog.is_some() {
            self.dialog_click(ctx, area, x, y);
            return true;
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
        if plan.text.contains(x, y) {
            let metrics = self.metrics(ctx, &plan);
            let row = (self.top + ((y - plan.text.y - ctx.px(6) as i32).max(0) / metrics.line_h) as usize).min(self.lines.len() - 1);
            let col = self.col_at(&metrics, row, x);
            self.move_caret(row, col, false);
            return true;
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        if let Some(dialog) = self.dialog.as_mut() {
            let last = dialog.entries.len().saturating_sub(1);
            dialog.top = if clicks > 0 { dialog.top.saturating_sub(3) } else { (dialog.top + 3).min(last) };
            return true;
        }
        let rows = self.page_rows.get();
        let last = self.lines.len().saturating_sub(rows.max(1));
        self.top = if clicks > 0 { self.top.saturating_sub(3) } else { (self.top + 3).min(last) };
        true
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        false
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

impl Editor {
    fn dialog_click(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) {
        let plan = dialog_plan(ctx, area);
        if plan.ok.contains(x, y) {
            self.dialog_accept();
            return;
        }
        if plan.cancel.contains(x, y) {
            self.dialog = None;
            return;
        }
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if plan.list.contains(x, y) {
            let offset = ((y - plan.list.y - ctx.px(2) as i32).max(0) as u32 / ctx.px(ITEM_H)) as usize;
            let index = dialog.top + offset;
            let Some((name, folder)) = dialog.entries.get(index).cloned() else {
                return;
            };
            let again = dialog.last_click == Some(index);
            dialog.selected = index;
            dialog.last_click = Some(index);
            if folder {
                // Папка — с одного щелчка, как ссылка: в окне выбора файла
                // зайти в неё и есть дело щелчка.
                dialog.enter(&name);
            } else {
                dialog.name = name;
                if again {
                    self.dialog_accept();
                }
            }
        }
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

/// Домашняя папка: `/root` у нулевого uid, иначе своя в `/home`.
fn home_dir() -> String {
    let me = user_progs::uid();
    if me == 0 {
        return String::from("/root");
    }
    for (name, folder) in list_dir("/home") {
        if !folder || name == ".." {
            continue;
        }
        let path = join("/home", &name);
        let mut info = Stat::default();
        if stat(&path, &mut info) >= 0 && info.uid == me {
            return path;
        }
    }
    String::from("/home")
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
    let path = args.get(1).map(String::from);
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    app::run(&SPEC, &info, size, move |_, _| Editor::new(path.as_deref()))
}
