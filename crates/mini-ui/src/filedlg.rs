//! Окно «Открыть» / «Сохранить как» (20.11): одно на все программы.
//!
//! # Зачем общее
//!
//! У Блокнота было своё окно выбора файла — список имён одной папки и поле
//! внизу. Роман (20.11): «странный юниксовый выбор папки, даже Рабочий стол не
//! найти». Человек с Windows ждёт привычное: слева быстрый доступ (Рабочий
//! стол, Документы, Загрузки…), сверху адрес с «хлебными крошками», справа
//! таблица с датой, типом и размером, внизу имя файла и тип. Такое окно нужно
//! каждой программе, которая что-то открывает или сохраняет, и копия в каждой
//! разошлась бы, как разошлись когда-то меню (см. [`crate::kit`]). Поэтому оно
//! здесь, в общем наборе.
//!
//! # Как устроено
//!
//! Тот же немедленный режим, что у [`crate::kit`]: окно — карточка поверх окна
//! программы (модально, остальное затемнено), а не отдельное окно стола.
//! Системных вызовов у этого крейта нет (его рисует и ядро), поэтому всё, что
//! касается диска и часов, окно спрашивает у [`Host`]. Готовая реализация для
//! программ — `user_progs::filedlg::SysHost`.
//!
//! # Как пользоваться (программа с окном на `user_progs::app`)
//!
//! ```ignore
//! use user_progs::filedlg::{FileDialog, Filter, Options, Outcome, SysHost};
//!
//! struct MyApp { host: SysHost, dialog: Option<FileDialog>, /* … */ }
//!
//! // Открыть «Сохранить как» (Ctrl+Shift+S):
//! let options = Options::save("Без имени.txt")
//!     .filters(vec![Filter::new("Текстовые файлы", &["txt"]), Filter::all()])
//!     .dir(current_folder);              // необязательно: иначе — последняя папка или «Документы»
//! self.dialog = Some(FileDialog::new(&self.host, options));
//!
//! // В App::draw — последним, поверх всего:
//! if let Some(d) = &self.dialog { d.draw(&self.host, s, ctx, area); }
//!
//! // Ввод: пока окно открыто, всё отдаётся ему.
//! //   key_with -> d.key(&self.host, code, mods, latin)
//! //   click    -> d.click(&self.host, ctx, area, x, y)
//! //   drag     -> d.drag(ctx, area, x, y)      (ползунок полосы прокрутки)
//! //   release  -> d.release()
//! //   wheel    -> d.wheel(clicks)
//! // Каждый из key/click возвращает Outcome:
//! //   Outcome::Pending        — окно ещё открыто;
//! //   Outcome::Cancelled      — «Отмена», крестик или Esc: закрыть окно;
//! //   Outcome::Chosen(path)   — полный путь: открыть или записать файл.
//! ```
//!
//! Перезапись существующего файла окно подтверждает само; расширение фильтра
//! («Добавлять расширение») дописывает само; папку, в которой выбрали файл,
//! запоминает через [`Host::remember_dir`] — следующее окно откроется в ней.
//!
//! # Клавиши
//!
//! Tab / Shift+Tab — поле имени → список → быстрый доступ → поиск; Enter —
//! открыть папку или выбрать файл; Esc — отмена; Alt+← / Alt+→ — назад и
//! вперёд; Alt+↑ или Backspace в списке — папка выше; F4, Alt+D, Ctrl+L —
//! править адрес; Ctrl+F, F3 — поиск; F5 — обновить; F2 — переименовать;
//! Ctrl+Shift+N — новая папка; стрелки, PageUp/PageDown, Home/End — по списку;
//! буква в списке — к имени на эту букву.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::iconset::{self, Fi};
use crate::kit;
use crate::paint::{self, Ctx, RowState, Weight};
use crate::typeface::{Face, Role};
use crate::{Color, Rect, Surface, draw, theme};

// ---------------------------------------------------------------------------
// Клавиши — коды договора окна (`user_abi::WIN_KEY_*`, `WIN_MOD_*`).
//
// Крейт не зависит от `user_abi` (его собирает и ядро), поэтому числа
// повторены здесь; `user_progs::filedlg` сверяет их с договором при сборке.
// ---------------------------------------------------------------------------

/// Первый код именованной клавиши.
pub const KEY_NAMED: u32 = 0x0100_0000;
pub const KEY_LEFT: u32 = KEY_NAMED + 1;
pub const KEY_RIGHT: u32 = KEY_NAMED + 2;
pub const KEY_UP: u32 = KEY_NAMED + 3;
pub const KEY_DOWN: u32 = KEY_NAMED + 4;
pub const KEY_HOME: u32 = KEY_NAMED + 5;
pub const KEY_END: u32 = KEY_NAMED + 6;
pub const KEY_PAGE_UP: u32 = KEY_NAMED + 7;
pub const KEY_PAGE_DOWN: u32 = KEY_NAMED + 8;
pub const KEY_DELETE: u32 = KEY_NAMED + 9;
pub const KEY_F1: u32 = KEY_NAMED + 12;
pub const MOD_SHIFT: i32 = 1;
pub const MOD_CTRL: i32 = 2;
pub const MOD_ALT: i32 = 4;

const KEY_F2: u32 = KEY_F1 + 1;
const KEY_F3: u32 = KEY_F1 + 2;
const KEY_F4: u32 = KEY_F1 + 3;
const KEY_F5: u32 = KEY_F1 + 4;
const ENTER: u32 = '\n' as u32;
const RETURN: u32 = '\r' as u32;
const TAB: u32 = '\t' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;

/// Псевдопапка «Этот компьютер»: тома и папки пользователя.
pub const COMPUTER: &str = "computer:";

/// Двойной щелчок — второй щелчок по тому же месту не позже этого.
const DOUBLE_CLICK_MS: u64 = 500;
/// Самое длинное имя и путь, которые примет поле.
const MAX_FIELD: usize = 255;

/// Папки пользователя: имя на диске, подпись, значок макета, значок набора.
const STANDARD: [(&str, &str, &str, Fi); 6] = [
    ("Desktop", "Рабочий стол", "folder-desktop", Fi::Monitor),
    ("Documents", "Документы", "folder-documents", Fi::FileText),
    ("Downloads", "Загрузки", "folder-downloads", Fi::Download),
    ("Pictures", "Изображения", "folder-pictures", Fi::Image),
    ("Music", "Музыка", "folder-music", Fi::Speaker),
    ("Videos", "Видео", "folder-videos", Fi::Play),
];

// ---------------------------------------------------------------------------
// Договор с программой
// ---------------------------------------------------------------------------

/// Открыть или сохранить.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Open,
    Save,
}

/// Тип файла в списке «Тип файла»: подпись и расширения (строчными, без
/// точки). Пустой список — «все файлы».
#[derive(Clone, Debug)]
pub struct Filter {
    pub label: String,
    pub exts: Vec<String>,
}

impl Filter {
    /// `Filter::new("Текстовые файлы", &["txt", "log"])` — подпись дополнится
    /// масками: «Текстовые файлы (*.txt; *.log)».
    #[must_use]
    pub fn new(label: &str, exts: &[&str]) -> Self {
        let masks: Vec<String> = exts.iter().map(|ext| format!("*.{ext}")).collect();
        let label = if masks.is_empty() { label.to_string() } else { format!("{label} ({})", masks.join("; ")) };
        Self { label, exts: exts.iter().map(|ext| ext.to_ascii_lowercase()).collect() }
    }

    /// «Все файлы (*.*)».
    #[must_use]
    pub fn all() -> Self {
        Self { label: String::from("Все файлы (*.*)"), exts: Vec::new() }
    }

    fn matches(&self, name: &str) -> bool {
        self.exts.is_empty() || self.exts.iter().any(|ext| extension(name) == *ext)
    }

    /// Расширение, которое дописывается к имени без расширения.
    fn default_ext(&self) -> Option<&str> {
        self.exts.first().map(String::as_str)
    }
}

/// С чем открыть окно.
#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    /// Заголовок; `None` — «Открыть» или «Сохранить как».
    pub title: Option<String>,
    /// Начальная папка; `None` (или папки нет) — последняя, где выбирали
    /// файл, иначе «Документы».
    pub dir: Option<String>,
    /// Имя файла по умолчанию (для сохранения — выделено целиком).
    pub name: String,
    pub filters: Vec<Filter>,
    /// Какой фильтр выбран сначала.
    pub filter: usize,
    /// Галочка «Добавлять расширение».
    pub add_extension: bool,
}

impl Options {
    /// «Открыть»: все файлы, без имени.
    #[must_use]
    pub fn open() -> Self {
        Self { mode: Mode::Open, title: None, dir: None, name: String::new(), filters: vec![Filter::all()], filter: 0, add_extension: false }
    }

    /// «Сохранить как» с именем по умолчанию.
    #[must_use]
    pub fn save(name: &str) -> Self {
        Self { mode: Mode::Save, title: None, dir: None, name: name.to_string(), filters: vec![Filter::all()], filter: 0, add_extension: true }
    }

    #[must_use]
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    #[must_use]
    pub fn dir(mut self, dir: &str) -> Self {
        self.dir = Some(dir.to_string());
        self
    }

    #[must_use]
    pub fn filters(mut self, filters: Vec<Filter>) -> Self {
        if !filters.is_empty() {
            self.filters = filters;
        }
        self
    }

    #[must_use]
    pub fn filter(mut self, index: usize) -> Self {
        self.filter = index;
        self
    }
}

/// Чем кончилось действие человека.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Окно ещё открыто.
    Pending,
    /// Отмена: закрыть окно, ничего не делая.
    Cancelled,
    /// Выбран файл: полный путь.
    Chosen(String),
}

/// Запись папки.
#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Секунды эпохи Unix; ноль — неизвестно.
    pub mtime: u32,
}

/// Подключённый том для «Этого компьютера».
#[derive(Clone, Debug)]
pub struct Volume {
    pub path: String,
    pub title: String,
    /// Значок макета: `place-system-disk`, `place-usb-stick`, `place-local-disk`.
    pub icon: &'static str,
    /// Байт всего и свободно (ноль — неизвестно).
    pub total: u64,
    pub free: u64,
}

/// Всё, что окну нужно от системы. Программам — `user_progs::filedlg::SysHost`.
pub trait Host {
    /// Содержимое папки без `.` и `..`.
    fn list(&self, dir: &str) -> Result<Vec<Item>, String>;
    /// `Some(true)` — папка, `Some(false)` — файл, `None` — нет такого.
    fn stat(&self, path: &str) -> Option<bool>;
    fn make_dir(&self, path: &str) -> Result<(), String>;
    fn rename(&self, from: &str, to: &str) -> Result<(), String>;
    /// Домашняя папка того, кто запустил программу.
    fn home(&self) -> String;
    /// Миллисекунды монотонных часов — для двойного щелчка.
    fn now_ms(&self) -> u64;
    /// Тома для «Этого компьютера».
    fn volumes(&self) -> Vec<Volume> {
        vec![Volume { path: String::from("/"), title: String::from("Системный диск"), icon: "place-system-disk", total: 0, free: 0 }]
    }
    /// Значок макета и тип файла по имени («Текстовый документ»).
    fn kind(&self, _name: &str, dir: bool) -> (&'static str, &'static str) {
        if dir { ("folder", "Папка с файлами") } else { ("doc-unknown", "Файл") }
    }
    /// Дата изменения словами: «20.11.2026 01:17».
    fn date(&self, _mtime: u32) -> String {
        String::new()
    }
    /// Нарисовать значок макета `name` в квадрат `rect`; `false` — нет такого
    /// (окно нарисует свой, из набора [`crate::iconset`]).
    fn draw_icon(&self, _s: &mut Surface, _rect: Rect, _name: &str) -> bool {
        false
    }
    /// Папка, где выбирали файл в прошлый раз.
    fn last_dir(&self) -> Option<String> {
        None
    }
    fn remember_dir(&self, _dir: &str) {}
    /// Строка в журнал (стенд ждёт по ним).
    fn log(&self, _line: &str) {}
}

// ---------------------------------------------------------------------------
// Поле ввода в одну строку
// ---------------------------------------------------------------------------

/// Строка с кареткой и выделением. Позиции — в символах, не байтах.
#[derive(Clone, Default, Debug)]
struct Line {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

impl Line {
    fn new(text: &str) -> Self {
        Self { text: text.to_string(), caret: text.chars().count(), anchor: None }
    }

    /// Строка, выделенная целиком (имя по умолчанию, адрес при правке).
    fn selected(text: &str) -> Self {
        let mut line = Self::new(text);
        if !text.is_empty() {
            line.anchor = Some(0);
        }
        line
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, at: usize) -> usize {
        self.text.char_indices().nth(at).map_or(self.text.len(), |(index, _)| index)
    }

    fn range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.caret).then(|| (anchor.min(self.caret), anchor.max(self.caret)))
    }

    /// Стереть выделенное. `true` — было что стирать.
    fn take_selection(&mut self) -> bool {
        let range = self.range();
        self.anchor = None;
        let Some((from, to)) = range else {
            return false;
        };
        let (a, b) = (self.byte(from), self.byte(to));
        self.text.replace_range(a..b, "");
        self.caret = from;
        true
    }

    fn insert(&mut self, ch: char) {
        self.take_selection();
        if self.text.len() + ch.len_utf8() > MAX_FIELD {
            return;
        }
        let at = self.byte(self.caret);
        self.text.insert(at, ch);
        self.caret += 1;
    }

    fn move_to(&mut self, at: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = at.min(self.len());
    }

    /// Клавиша правки. `None` — не своя; `Some(changed)` — разобрана, и
    /// `changed` — поменялся ли текст.
    fn key(&mut self, code: u32, mods: i32, latin: u32) -> Option<bool> {
        let shift = mods & MOD_SHIFT != 0;
        let ctrl = mods & MOD_CTRL != 0;
        match code {
            BACKSPACE => {
                if !self.take_selection() && self.caret > 0 {
                    let (a, b) = (self.byte(self.caret - 1), self.byte(self.caret));
                    self.text.replace_range(a..b, "");
                    self.caret -= 1;
                }
                Some(true)
            }
            KEY_DELETE => {
                if !self.take_selection() && self.caret < self.len() {
                    let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
                    self.text.replace_range(a..b, "");
                }
                Some(true)
            }
            KEY_LEFT => {
                match (shift, self.range()) {
                    (false, Some((from, _))) => self.move_to(from, false),
                    _ => self.move_to(self.caret.saturating_sub(1), shift),
                }
                Some(false)
            }
            KEY_RIGHT => {
                match (shift, self.range()) {
                    (false, Some((_, to))) => self.move_to(to, false),
                    _ => self.move_to(self.caret + 1, shift),
                }
                Some(false)
            }
            KEY_HOME => {
                self.move_to(0, shift);
                Some(false)
            }
            KEY_END => {
                self.move_to(self.len(), shift);
                Some(false)
            }
            _ if ctrl && latin_is(latin, 'a') => {
                self.anchor = Some(0);
                self.caret = self.len();
                Some(false)
            }
            code if code < KEY_NAMED && !ctrl => match char::from_u32(code).filter(|ch| !ch.is_control()) {
                Some(ch) => {
                    self.insert(ch);
                    Some(true)
                }
                None => None,
            },
            _ => None,
        }
    }

    /// Первый показанный символ: каретка всегда в поле.
    fn first_shown(&self, face: &Face, room: u32) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut first = 0;
        loop {
            let width: u32 = chars[first..self.caret.min(chars.len())].iter().map(|ch| char_width(face, *ch)).sum();
            if width <= room || first >= self.caret {
                return first;
            }
            first += 1;
        }
    }

    /// Каретку — к точке `x` (от левого края текста).
    fn click(&mut self, face: &Face, room: u32, x: i32) {
        let first = self.first_shown(face, room);
        let mut pen = 0i32;
        let mut at = first;
        for ch in self.text.chars().skip(first) {
            let w = char_width(face, ch) as i32;
            if pen + w / 2 > x {
                break;
            }
            pen += w;
            at += 1;
        }
        self.anchor = None;
        self.caret = at.min(self.len());
    }

    /// Нарисовать текст в `rect` (уже с полями): выделение, каретку.
    fn draw(&self, ctx: Ctx, s: &mut Surface, rect: Rect, focused: bool, ink: Color) {
        let p = ctx.palette;
        let face = ctx.face(Role::Body);
        let first = self.first_shown(face, rect.w.saturating_sub(ctx.px(2)));
        let visible: String = self.text.chars().skip(first).collect();
        let x_of = |at: usize| -> i32 {
            let w: u32 = self.text.chars().skip(first).take(at.saturating_sub(first)).map(|ch| char_width(face, ch)).sum();
            rect.x + w.min(rect.w) as i32
        };
        let y = paint::baseline(ctx, Role::Body, rect);
        if focused {
            if let Some((from, to)) = self.range() {
                let (a, b) = (x_of(from.max(first)), x_of(to.max(first)));
                if b > a {
                    let h = u32::from(face.line) + ctx.px(4);
                    draw::rounded(s, Rect::new(a, y - ctx.px(2) as i32, (b - a) as u32, h), ctx.px(2), p.acc, 150);
                }
            }
        }
        paint::text_clipped(ctx, s, Role::Body, rect.x, y, rect.w, &visible, ink);
        if focused && self.range().is_none() {
            let x = x_of(self.caret);
            s.fill(Rect::new(x, y - ctx.px(1) as i32, ctx.px(1).max(2), u32::from(face.line) + ctx.px(2)), p.ink);
        }
    }
}

fn char_width(face: &Face, ch: char) -> u32 {
    let mut buffer = [0u8; 4];
    face.width(ch.encode_utf8(&mut buffer))
}

fn latin_is(latin: u32, letter: char) -> bool {
    char::from_u32(latin).is_some_and(|ch| ch.to_ascii_lowercase() == letter)
}

// ---------------------------------------------------------------------------
// Состояние окна
// ---------------------------------------------------------------------------

/// Строка списка.
#[derive(Clone, Debug)]
struct Row {
    label: String,
    /// Подпись строчными — для сортировки, поиска и перехода по букве.
    key: String,
    path: String,
    dir: bool,
    size: u64,
    mtime: u32,
    icon: &'static str,
    kind: String,
    date: String,
    /// Том на «Этом компьютере»: размер — ёмкость.
    volume: bool,
    /// Свободно на томе, байт.
    free: u64,
}

/// Место в колонке быстрого доступа.
#[derive(Clone)]
struct Place {
    label: String,
    path: String,
    icon: &'static str,
    fi: Fi,
    indent: bool,
    /// Черта над местом — начало группы.
    gap: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Name,
    List,
    Places,
    Search,
    Address,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Column {
    Name,
    Date,
    Kind,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Details,
    Icons,
}

/// Что под последним щелчком — для двойного щелчка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Row(usize),
    Place(usize),
}

/// Окно «Открыть» / «Сохранить как». См. заголовок модуля.
pub struct FileDialog {
    mode: Mode,
    title: String,
    dir: String,
    home: String,
    rows: Vec<Row>,
    /// Номера строк `rows`, прошедших поиск, в порядке сортировки.
    shown: Vec<usize>,
    /// Выбранная строка — номер в `shown`.
    selected: Option<usize>,
    /// Первая показанная линия (строка таблицы или ряд значков).
    top: usize,
    back: Vec<String>,
    forward: Vec<String>,
    name: Line,
    search: Line,
    /// Адрес в правке (щелчок по адресной строке, F4).
    address: Option<Line>,
    filters: Vec<Filter>,
    filter: usize,
    add_ext: bool,
    sort: Column,
    ascending: bool,
    view: View,
    focus: Focus,
    places: Vec<Place>,
    place_cursor: usize,
    /// Ошибка под списком (красным).
    error: Option<String>,
    /// Файл есть — путь ждёт ответа «Заменить?».
    confirm: Option<String>,
    /// Открыт список «Тип файла»: подсвеченный пункт.
    type_menu: Option<usize>,
    /// Переименование строки (новая папка): номер в `rows` и поле.
    rename: Option<(usize, Line)>,
    last_click: Option<(Target, u64)>,
    /// Тянут ползунок прокрутки: где его схватили (от верха ползунка).
    grab: Option<i32>,
    /// Раскладка последней отрисовки — клавишам (PageDown листает ровно на
    /// экран, а сколько это — знает только отрисовка).
    geometry: Cell<Option<(Ctx, Rect)>>,
}

impl FileDialog {
    /// Открыть окно. Недостающие папки пользователя (Рабочий стол, Документы…)
    /// создаются здесь же: быстрый доступ без них был бы пустым.
    #[must_use]
    pub fn new(host: &dyn Host, options: Options) -> Self {
        let home = host.home();
        // Дома нет (живая система: `/root` никто не заводил) — завести, как
        // заводит его стол для «Рабочего стола».
        if home != "/" && host.stat(&home).is_none() {
            match host.make_dir(&home) {
                Ok(()) => host.log(&format!("filedlg: created {home}")),
                Err(why) => host.log(&format!("filedlg: cannot create {home}: {why}")),
            }
        }
        if home != "/" && host.stat(&home) == Some(true) {
            for (folder, ..) in STANDARD {
                let path = join(&home, folder);
                if host.stat(&path).is_none() {
                    match host.make_dir(&path) {
                        Ok(()) => host.log(&format!("filedlg: created {path}")),
                        Err(why) => host.log(&format!("filedlg: cannot create {path}: {why}")),
                    }
                }
            }
        }
        let start = [options.dir.clone(), host.last_dir(), Some(join(&home, "Documents")), Some(home.clone())]
            .into_iter()
            .flatten()
            .find(|dir| host.stat(dir) == Some(true))
            .unwrap_or_else(|| String::from("/"));
        let title = options.title.clone().unwrap_or_else(|| String::from(if options.mode == Mode::Save { "Сохранить как" } else { "Открыть" }));
        let filters = if options.filters.is_empty() { vec![Filter::all()] } else { options.filters };
        let filter = options.filter.min(filters.len() - 1);
        let mut dialog = Self {
            mode: options.mode,
            title,
            dir: start,
            home,
            rows: Vec::new(),
            shown: Vec::new(),
            selected: None,
            top: 0,
            back: Vec::new(),
            forward: Vec::new(),
            name: if options.mode == Mode::Save { Line::selected(&options.name) } else { Line::new(&options.name) },
            search: Line::default(),
            address: None,
            filters,
            filter,
            add_ext: options.add_extension,
            sort: Column::Name,
            ascending: true,
            view: View::Details,
            focus: Focus::Name,
            places: Vec::new(),
            place_cursor: 0,
            error: None,
            confirm: None,
            type_menu: None,
            rename: None,
            last_click: None,
            grab: None,
            geometry: Cell::new(None),
        };
        dialog.places = dialog.make_places(host);
        dialog.reload(host);
        host.log(&format!("filedlg: {} at {}", if dialog.mode == Mode::Save { "save" } else { "open" }, dialog.dir));
        dialog
    }

    /// Текущая папка.
    #[must_use]
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// Имя в поле «Имя файла».
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name.text
    }

    fn make_places(&self, host: &dyn Host) -> Vec<Place> {
        let mut out = Vec::new();
        if self.home != "/" && host.stat(&self.home) == Some(true) {
            out.push(Place { label: String::from("Главная"), path: self.home.clone(), icon: "folder", fi: Fi::Home, indent: false, gap: false });
            for (folder, label, icon, fi) in STANDARD {
                let path = join(&self.home, folder);
                if host.stat(&path) == Some(true) {
                    out.push(Place { label: label.to_string(), path, icon, fi, indent: false, gap: false });
                }
            }
        }
        let gap = !out.is_empty();
        out.push(Place { label: String::from("Этот компьютер"), path: String::from(COMPUTER), icon: "place-this-pc", fi: Fi::Laptop, indent: false, gap });
        for volume in host.volumes() {
            let fi = if volume.icon == "place-usb-stick" { Fi::Usb } else { Fi::Drive };
            out.push(Place { label: volume.title, path: volume.path, icon: volume.icon, fi, indent: true, gap: false });
        }
        out
    }

    // -----------------------------------------------------------------------
    // Папка
    // -----------------------------------------------------------------------

    /// Перечитать папку: строки, фильтр типа, поиск, сортировка.
    fn reload(&mut self, host: &dyn Host) {
        self.rows.clear();
        self.error = None;
        if self.dir == COMPUTER {
            let volumes = host.volumes();
            for place in self.places.iter().filter(|place| place.path != COMPUTER && place.path != self.home) {
                let volume = place.indent;
                let found = volumes.iter().find(|v| v.path == place.path).filter(|_| volume);
                let (size, free) = found.map_or((0, 0), |v| (v.total, v.free));
                let kind = match found {
                    Some(v) if v.icon == "place-usb-stick" => String::from("Съёмный диск"),
                    Some(_) => String::from("Локальный диск"),
                    None => String::from("Папка с файлами"),
                };
                self.rows.push(Row {
                    label: place.label.clone(),
                    key: lower(&place.label),
                    path: place.path.clone(),
                    dir: true,
                    size,
                    mtime: 0,
                    icon: place.icon,
                    kind,
                    date: String::new(),
                    volume,
                    free,
                });
            }
        } else {
            match host.list(&self.dir) {
                Ok(items) => {
                    let filter = &self.filters[self.filter];
                    for item in items {
                        if item.name.starts_with('.') || (!item.dir && !filter.matches(&item.name)) {
                            continue;
                        }
                        let (icon, kind) = host.kind(&item.name, item.dir);
                        let label = display_name(&self.dir, &self.home, &item.name);
                        self.rows.push(Row {
                            key: lower(&label),
                            label,
                            path: join(&self.dir, &item.name),
                            dir: item.dir,
                            size: item.size,
                            mtime: item.mtime,
                            icon,
                            kind: kind.to_string(),
                            date: host.date(item.mtime),
                            volume: false,
                            free: 0,
                        });
                    }
                }
                Err(why) => self.error = Some(format!("Папка не открывается: {why}")),
            }
        }
        self.refilter();
    }

    /// Поиск и сортировка поверх прочитанного; выбор — сначала.
    fn refilter(&mut self) {
        let needle = lower(self.search.text.trim());
        let mut shown: Vec<usize> = (0..self.rows.len()).filter(|index| needle.is_empty() || self.rows[*index].key.contains(&needle)).collect();
        let rows = &self.rows;
        let (sort, ascending) = (self.sort, self.ascending);
        shown.sort_by(|a, b| {
            let (a, b) = (&rows[*a], &rows[*b]);
            // Папки — всегда первыми, в какую сторону ни сортируй.
            b.dir.cmp(&a.dir).then_with(|| {
                let order = match sort {
                    Column::Name => a.key.cmp(&b.key),
                    Column::Date => a.mtime.cmp(&b.mtime).then_with(|| a.key.cmp(&b.key)),
                    Column::Kind => a.kind.cmp(&b.kind).then_with(|| a.key.cmp(&b.key)),
                    Column::Size => a.size.cmp(&b.size).then_with(|| a.key.cmp(&b.key)),
                };
                if ascending { order } else { order.reverse() }
            })
        });
        self.shown = shown;
        self.selected = None;
        self.top = 0;
    }

    fn go(&mut self, host: &dyn Host, dir: &str) {
        if dir == self.dir {
            self.reload(host);
            return;
        }
        if dir != COMPUTER && host.stat(dir) != Some(true) {
            self.error = Some(format!("Папка «{dir}» не найдена"));
            return;
        }
        let previous = core::mem::replace(&mut self.dir, dir.to_string());
        self.back.push(previous);
        self.forward.clear();
        self.after_move(host);
    }

    fn after_move(&mut self, host: &dyn Host) {
        self.search = Line::default();
        self.address = None;
        self.rename = None;
        self.reload(host);
        if self.focus == Focus::Search || self.focus == Focus::Address {
            self.focus = Focus::List;
        }
        host.log(&format!("filedlg: at {}", self.dir));
    }

    fn go_back(&mut self, host: &dyn Host) {
        if let Some(dir) = self.back.pop() {
            let current = core::mem::replace(&mut self.dir, dir);
            self.forward.push(current);
            self.after_move(host);
        }
    }

    fn go_forward(&mut self, host: &dyn Host) {
        if let Some(dir) = self.forward.pop() {
            let current = core::mem::replace(&mut self.dir, dir);
            self.back.push(current);
            self.after_move(host);
        }
    }

    fn go_up(&mut self, host: &dyn Host) {
        if self.dir != COMPUTER {
            let up = if self.dir == "/" { String::from(COMPUTER) } else { parent_of(&self.dir) };
            let child = self.dir.clone();
            self.go(host, &up);
            // Папка, из которой вышли, — выбрана: так в Проводнике.
            if let Some(at) = self.shown.iter().position(|index| self.rows[*index].path == child) {
                self.select(at);
            }
        }
    }

    fn new_folder(&mut self, host: &dyn Host) {
        if self.dir == COMPUTER {
            self.error = Some(String::from("Здесь папку не создать — откройте диск или папку"));
            return;
        }
        let mut name = String::from("Новая папка");
        let mut number = 2;
        while host.stat(&join(&self.dir, &name)).is_some() {
            name = format!("Новая папка ({number})");
            number += 1;
        }
        let path = join(&self.dir, &name);
        if let Err(why) = host.make_dir(&path) {
            self.error = Some(format!("Папку не создать: {why}"));
            return;
        }
        host.log(&format!("filedlg: new folder {path}"));
        self.search = Line::default();
        self.reload(host);
        if let Some(index) = self.rows.iter().position(|row| row.path == path) {
            if let Some(at) = self.shown.iter().position(|shown| *shown == index) {
                self.select(at);
            }
            self.rename = Some((index, Line::selected(&name)));
            self.focus = Focus::List;
        }
    }

    /// Закончить переименование: `keep` — принять набранное.
    fn finish_rename(&mut self, host: &dyn Host, keep: bool) {
        let Some((index, line)) = self.rename.take() else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let new = line.text.trim();
        if !keep || new.is_empty() || new == row.label {
            return;
        }
        if new.contains('/') {
            self.error = Some(String::from("Имя не может содержать «/»"));
            return;
        }
        let from = row.path.clone();
        let to = join(&self.dir, new);
        match host.rename(&from, &to) {
            Ok(()) => {
                host.log(&format!("filedlg: renamed {from} -> {to}"));
                self.reload(host);
                if let Some(at) = self.shown.iter().position(|index| self.rows[*index].path == to) {
                    self.select(at);
                }
            }
            Err(why) => self.error = Some(format!("Не переименовать: {why}")),
        }
    }

    fn select(&mut self, at: usize) {
        if self.shown.is_empty() {
            self.selected = None;
            return;
        }
        let at = at.min(self.shown.len() - 1);
        self.selected = Some(at);
        let (per_line, lines) = self.page();
        let line = at / per_line;
        if line < self.top {
            self.top = line;
        } else if line >= self.top + lines {
            self.top = line + 1 - lines;
        }
    }

    fn selected_row(&self) -> Option<&Row> {
        self.selected.and_then(|at| self.shown.get(at)).map(|index| &self.rows[*index])
    }

    /// Выбрали строку: файл кладёт своё имя в поле.
    fn take_name_from_selection(&mut self) {
        if let Some(row) = self.selected_row() {
            if !row.dir {
                let name = row.label.clone();
                self.name = if self.mode == Mode::Save { Line::selected(&name) } else { Line::new(&name) };
            }
        }
    }

    // -----------------------------------------------------------------------
    // Готово
    // -----------------------------------------------------------------------

    /// Открыть выбранную строку: папку — войти, файл — выбрать.
    fn activate(&mut self, host: &dyn Host) -> Outcome {
        let Some(row) = self.selected_row().cloned() else {
            return self.accept(host);
        };
        if row.dir {
            self.go(host, &row.path);
            return Outcome::Pending;
        }
        self.name = Line::new(&row.label);
        self.accept(host)
    }

    /// «Сохранить» / «Открыть».
    fn accept(&mut self, host: &dyn Host) -> Outcome {
        self.error = None;
        let typed = self.name.text.trim().to_string();
        if typed.is_empty() {
            if let Some(row) = self.selected_row().cloned() {
                if row.dir {
                    self.go(host, &row.path);
                    return Outcome::Pending;
                }
                if self.mode == Mode::Open {
                    self.name = Line::new(&row.label);
                    return self.accept(host);
                }
            }
            self.error = Some(String::from("Введите имя файла"));
            self.focus = Focus::Name;
            return Outcome::Pending;
        }
        let mut path = if typed.starts_with('/') {
            typed.clone()
        } else if let Some(rest) = typed.strip_prefix("~/") {
            join(&self.home, rest)
        } else if typed == "~" {
            self.home.clone()
        } else if self.dir == COMPUTER {
            self.error = Some(String::from("Выберите папку для файла"));
            return Outcome::Pending;
        } else {
            join(&self.dir, &typed)
        };
        path = normalize(&path);
        // Имя папки — войти в неё, как в Windows.
        if host.stat(&path) == Some(true) {
            self.name = Line::default();
            self.go(host, &path);
            return Outcome::Pending;
        }
        let leaf = path.rsplit('/').next().unwrap_or("").to_string();
        if self.mode == Mode::Open {
            return match host.stat(&path) {
                Some(false) => self.chosen(host, path),
                _ => {
                    self.error = Some(format!("Файл «{leaf}» не найден. Проверьте имя файла."));
                    Outcome::Pending
                }
            };
        }
        if self.add_ext && extension(&leaf).is_empty() {
            if let Some(ext) = self.filters[self.filter].default_ext() {
                path = format!("{path}.{ext}");
            }
        }
        match host.stat(&path) {
            Some(true) => {
                self.name = Line::default();
                self.go(host, &path);
                Outcome::Pending
            }
            Some(false) => {
                host.log(&format!("filedlg: asking to replace {path}"));
                self.confirm = Some(path);
                Outcome::Pending
            }
            None => {
                let parent = parent_of(&path);
                if host.stat(&parent) != Some(true) {
                    self.error = Some(format!("Папка «{parent}» не найдена"));
                    return Outcome::Pending;
                }
                self.chosen(host, path)
            }
        }
    }

    fn chosen(&mut self, host: &dyn Host, path: String) -> Outcome {
        host.remember_dir(&parent_of(&path));
        host.log(&format!("filedlg: chosen {path}"));
        Outcome::Chosen(path)
    }

    // -----------------------------------------------------------------------
    // Клавиши
    // -----------------------------------------------------------------------

    /// Клавиша (код, `MOD_*`, латинская буква клавиши — как в `App::key_with`).
    pub fn key(&mut self, host: &dyn Host, code: u32, mods: i32, latin: u32) -> Outcome {
        let ctrl = mods & MOD_CTRL != 0;
        let alt = mods & MOD_ALT != 0;
        let shift = mods & MOD_SHIFT != 0;
        let enter = code == ENTER || code == RETURN;

        if let Some(path) = self.confirm.clone() {
            // «Да» — Enter или Y, «Нет» — Esc или N (по латинской букве
            // клавиши: в русской раскладке те же клавиши).
            if enter || latin_is(latin, 'y') {
                self.confirm = None;
                return self.chosen(host, path);
            }
            if code == ESCAPE || latin_is(latin, 'n') {
                self.confirm = None;
                self.focus = Focus::Name;
            }
            return Outcome::Pending;
        }
        if let Some(cursor) = self.type_menu {
            match code {
                KEY_UP => self.type_menu = Some(cursor.saturating_sub(1)),
                KEY_DOWN => self.type_menu = Some((cursor + 1).min(self.filters.len() - 1)),
                _ if enter => self.set_filter(host, cursor),
                _ => self.type_menu = None,
            }
            return Outcome::Pending;
        }
        if let Some((_, line)) = self.rename.as_mut() {
            match code {
                ESCAPE => self.finish_rename(host, false),
                _ if enter => self.finish_rename(host, true),
                _ => {
                    line.key(code, mods, latin);
                }
            }
            return Outcome::Pending;
        }

        // Сочетания окна — при любом фокусе.
        match code {
            ESCAPE => {
                if self.address.take().is_some() {
                    self.focus = Focus::List;
                    return Outcome::Pending;
                }
                return Outcome::Cancelled;
            }
            TAB => {
                self.address = None;
                let order = [Focus::Name, Focus::List, Focus::Places, Focus::Search];
                let at = order.iter().position(|focus| *focus == self.focus).unwrap_or(0);
                self.focus = order[if shift { (at + order.len() - 1) % order.len() } else { (at + 1) % order.len() }];
                if self.focus == Focus::List && self.selected.is_none() && !self.shown.is_empty() {
                    self.select(0);
                }
                if self.focus == Focus::Places {
                    self.place_cursor = self.places.iter().position(|place| place.path == self.dir).unwrap_or(0);
                }
                return Outcome::Pending;
            }
            KEY_LEFT if alt => {
                self.go_back(host);
                return Outcome::Pending;
            }
            KEY_RIGHT if alt => {
                self.go_forward(host);
                return Outcome::Pending;
            }
            KEY_UP if alt => {
                self.go_up(host);
                return Outcome::Pending;
            }
            KEY_F5 => {
                self.reload(host);
                return Outcome::Pending;
            }
            KEY_F3 => {
                self.focus = Focus::Search;
                return Outcome::Pending;
            }
            KEY_F4 => {
                self.edit_address();
                return Outcome::Pending;
            }
            _ if (alt && latin_is(latin, 'd')) || (ctrl && latin_is(latin, 'l')) => {
                self.edit_address();
                return Outcome::Pending;
            }
            _ if ctrl && latin_is(latin, 'f') => {
                self.focus = Focus::Search;
                return Outcome::Pending;
            }
            _ if ctrl && shift && latin_is(latin, 'n') => {
                self.new_folder(host);
                return Outcome::Pending;
            }
            _ => {}
        }

        match self.focus {
            Focus::Address => {
                if enter {
                    let text = self.address.as_ref().map(|line| line.text.trim().to_string()).unwrap_or_default();
                    let target = if text == "~" {
                        self.home.clone()
                    } else if let Some(rest) = text.strip_prefix("~/") {
                        join(&self.home, rest)
                    } else if text == "Этот компьютер" || text == COMPUTER {
                        String::from(COMPUTER)
                    } else {
                        normalize(&text)
                    };
                    if target == COMPUTER || host.stat(&target) == Some(true) {
                        self.address = None;
                        self.focus = Focus::List;
                        self.go(host, &target);
                    } else {
                        self.error = Some(format!("Путь «{text}» не найден"));
                    }
                } else if let Some(line) = self.address.as_mut() {
                    line.key(code, mods, latin);
                }
            }
            Focus::Search => {
                if enter || code == KEY_DOWN {
                    self.focus = Focus::List;
                    if !self.shown.is_empty() {
                        self.select(0);
                    }
                } else if self.search.key(code, mods, latin) == Some(true) {
                    self.refilter();
                }
            }
            Focus::Places => match code {
                KEY_UP => self.place_cursor = self.place_cursor.saturating_sub(1),
                KEY_DOWN => self.place_cursor = (self.place_cursor + 1).min(self.places.len().saturating_sub(1)),
                _ if enter || code == ' ' as u32 => {
                    if let Some(place) = self.places.get(self.place_cursor).cloned() {
                        self.go(host, &place.path);
                        self.focus = Focus::Places;
                    }
                }
                _ => {}
            },
            Focus::List => {
                if enter {
                    return self.activate(host);
                }
                match code {
                    BACKSPACE => self.go_up(host),
                    KEY_F2 => {
                        if let Some(at) = self.selected.and_then(|at| self.shown.get(at)).copied() {
                            if self.dir != COMPUTER {
                                self.rename = Some((at, Line::selected(&self.rows[at].label)));
                            }
                        }
                    }
                    _ if self.list_move(code) => self.take_name_from_selection(),
                    code if code < KEY_NAMED && !ctrl => {
                        // Буква — к следующему имени на эту букву.
                        if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                            let letter = lower(ch.encode_utf8(&mut [0u8; 4]));
                            let count = self.shown.len();
                            let from = self.selected.map_or(0, |at| at + 1);
                            if let Some(at) = (0..count).map(|step| (from + step) % count.max(1)).find(|at| self.rows[self.shown[*at]].key.starts_with(&letter)) {
                                self.select(at);
                                self.take_name_from_selection();
                            }
                        }
                    }
                    _ => {}
                }
            }
            Focus::Name => {
                if enter {
                    return self.accept(host);
                }
                if matches!(code, KEY_UP | KEY_DOWN | KEY_PAGE_UP | KEY_PAGE_DOWN) {
                    if self.list_move(code) {
                        self.take_name_from_selection();
                    }
                } else if self.name.key(code, mods, latin).is_some() {
                    self.error = None;
                }
            }
        }
        Outcome::Pending
    }

    /// Стрелки и листание по списку. `true` — выбор сдвинулся.
    fn list_move(&mut self, code: u32) -> bool {
        let count = self.shown.len();
        if count == 0 {
            return false;
        }
        let (per_line, lines) = self.page();
        let current = self.selected;
        let at = current.unwrap_or(0);
        let next = match code {
            KEY_UP if current.is_none() => 0,
            KEY_DOWN if current.is_none() => 0,
            KEY_UP => at.saturating_sub(per_line),
            KEY_DOWN => (at + per_line).min(count - 1),
            KEY_LEFT if per_line > 1 => at.saturating_sub(1),
            KEY_RIGHT if per_line > 1 => (at + 1).min(count - 1),
            KEY_PAGE_UP => at.saturating_sub(per_line * lines.max(1)),
            KEY_PAGE_DOWN => (at + per_line * lines.max(1)).min(count - 1),
            KEY_HOME => 0,
            KEY_END => count - 1,
            _ => return false,
        };
        self.select(next);
        true
    }

    fn edit_address(&mut self) {
        let text = if self.dir == COMPUTER { String::from("Этот компьютер") } else { self.dir.clone() };
        self.address = Some(Line::selected(&text));
        self.focus = Focus::Address;
    }

    fn set_filter(&mut self, host: &dyn Host, index: usize) {
        self.type_menu = None;
        if index >= self.filters.len() || index == self.filter {
            return;
        }
        self.filter = index;
        // Сменили тип при сохранении — расширение имени следом, как в Windows.
        if self.mode == Mode::Save && self.add_ext {
            if let Some(ext) = self.filters[index].default_ext() {
                let name = self.name.text.clone();
                if let Some((stem, old)) = name.rsplit_once('.') {
                    if !stem.is_empty() && !old.is_empty() && old != ext {
                        self.name = Line::selected(&format!("{stem}.{ext}"));
                    }
                }
            }
        }
        host.log(&format!("filedlg: filter {}", self.filters[index].label));
        self.reload(host);
    }

    // -----------------------------------------------------------------------
    // Мышь
    // -----------------------------------------------------------------------

    /// Колесо: три строки таблицы или ряд значков за щелчок.
    pub fn wheel(&mut self, clicks: i32) {
        if self.type_menu.is_some() || self.confirm.is_some() {
            return;
        }
        let (per_line, lines) = self.page();
        let step = if per_line > 1 { 1 } else { 3 };
        let total = self.shown.len().div_ceil(per_line);
        let last = total.saturating_sub(lines);
        self.top = if clicks > 0 { self.top.saturating_sub(step * clicks as usize) } else { (self.top + step * clicks.unsigned_abs() as usize).min(last) };
    }

    /// Указатель движется с зажатой кнопкой: ползунок прокрутки.
    pub fn drag(&mut self, ctx: Ctx, area: Rect, _x: i32, y: i32) -> bool {
        let Some(grab) = self.grab else {
            return false;
        };
        let lay = Layout::new(ctx, area, self);
        let (per_line, lines) = lay.page(ctx, self.view);
        let total = self.shown.len().div_ceil(per_line);
        if let Some((track, thumb)) = scrollbar(&lay, total, lines, self.top) {
            let room = (track.h - thumb.h).max(1) as i32;
            let offset = (y - grab - track.y).clamp(0, room);
            let last = total.saturating_sub(lines);
            self.top = ((offset as usize * last + room as usize / 2) / room as usize).min(last);
        }
        true
    }

    /// Кнопку отпустили.
    pub fn release(&mut self) {
        self.grab = None;
    }

    /// Щелчок левой кнопкой в точке окна программы.
    pub fn click(&mut self, host: &dyn Host, ctx: Ctx, area: Rect, x: i32, y: i32) -> Outcome {
        let lay = Layout::new(ctx, area, self);
        let now = host.now_ms();

        if let Some(path) = self.confirm.clone() {
            let (_, yes, no) = confirm_layout(ctx, lay.card);
            if yes.contains(x, y) {
                self.confirm = None;
                return self.chosen(host, path);
            }
            if no.contains(x, y) {
                self.confirm = None;
                self.focus = Focus::Name;
            }
            return Outcome::Pending;
        }
        if self.type_menu.is_some() {
            let hit = {
                let (rect, entries) = self.type_menu_layout(ctx, &lay, area);
                kit::menu_hit(ctx, rect, &entries, x, y)
            };
            match hit {
                Some(index) => self.set_filter(host, index),
                None => self.type_menu = None,
            }
            return Outcome::Pending;
        }
        // Щелчок мимо переименования — принять набранное (как в Проводнике).
        if let Some((index, _)) = &self.rename {
            let index = *index;
            let inside = self.shown.iter().position(|at| *at == index).and_then(|at| lay.item_rect(ctx, self, at)).is_some_and(|rect| rect.contains(x, y));
            if inside {
                return Outcome::Pending;
            }
            self.finish_rename(host, true);
        }
        if !lay.address.contains(x, y) && self.address.is_some() {
            self.address = None;
            if self.focus == Focus::Address {
                self.focus = Focus::List;
            }
        }
        if !lay.card.contains(x, y) {
            return Outcome::Pending;
        }

        if lay.close.contains(x, y) || lay.cancel.contains(x, y) {
            return Outcome::Cancelled;
        }
        if lay.ok.contains(x, y) {
            return if self.focus == Focus::List && self.name.text.trim().is_empty() { self.activate(host) } else { self.accept(host) };
        }
        if lay.back.contains(x, y) {
            self.go_back(host);
            return Outcome::Pending;
        }
        if lay.forward.contains(x, y) {
            self.go_forward(host);
            return Outcome::Pending;
        }
        if lay.up.contains(x, y) {
            self.go_up(host);
            return Outcome::Pending;
        }
        if lay.refresh.contains(x, y) {
            self.reload(host);
            return Outcome::Pending;
        }
        if lay.new_folder.contains(x, y) {
            self.new_folder(host);
            return Outcome::Pending;
        }
        if lay.view_details.contains(x, y) || lay.view_icons.contains(x, y) {
            self.view = if lay.view_details.contains(x, y) { View::Details } else { View::Icons };
            self.top = 0;
            if let Some(at) = self.selected {
                self.select(at);
            }
            return Outcome::Pending;
        }
        if lay.address.contains(x, y) {
            if let Some(line) = self.address.as_mut() {
                let field = lay.address_text(ctx);
                line.click(ctx.face(Role::Body), field.w, x - field.x);
                return Outcome::Pending;
            }
            if let Some((_, path)) = self.crumbs(ctx, &lay).into_iter().find(|(rect, _)| rect.contains(x, y)) {
                self.go(host, &path);
                return Outcome::Pending;
            }
            self.edit_address();
            return Outcome::Pending;
        }
        if lay.search.contains(x, y) {
            self.focus = Focus::Search;
            let field = lay.search_text(ctx);
            self.search.click(ctx.face(Role::Body), field.w, x - field.x);
            return Outcome::Pending;
        }
        if lay.name.contains(x, y) {
            self.focus = Focus::Name;
            let field = field_text(ctx, lay.name);
            self.name.click(ctx.face(Role::Body), field.w, x - field.x);
            return Outcome::Pending;
        }
        if lay.kind.contains(x, y) {
            self.type_menu = Some(self.filter);
            return Outcome::Pending;
        }
        if self.mode == Mode::Save && lay.check.contains(x, y) {
            self.add_ext = !self.add_ext;
            return Outcome::Pending;
        }
        if lay.places.contains(x, y) {
            if let Some(at) = lay.place_rects(ctx, &self.places).iter().position(|rect| rect.contains(x, y)) {
                self.focus = Focus::Places;
                self.place_cursor = at;
                let path = self.places[at].path.clone();
                self.last_click = Some((Target::Place(at), now));
                self.go(host, &path);
            }
            return Outcome::Pending;
        }
        // Полоса прокрутки.
        let (per_line, lines) = lay.page(ctx, self.view);
        let total = self.shown.len().div_ceil(per_line);
        if let Some((track, thumb)) = scrollbar(&lay, total, lines, self.top) {
            if track.contains(x, y) {
                if thumb.contains(x, y) {
                    self.grab = Some(y - thumb.y);
                } else {
                    let last = total.saturating_sub(lines);
                    self.top = if y < thumb.y { self.top.saturating_sub(lines.max(1)) } else { (self.top + lines.max(1)).min(last) };
                }
                return Outcome::Pending;
            }
        }
        if self.view == View::Details && lay.header.contains(x, y) {
            let columns = lay.columns(ctx);
            for (rect, column) in columns.iter().zip([Column::Name, Column::Date, Column::Kind, Column::Size]) {
                if rect.w > 0 && x >= rect.x && x < rect.right() {
                    if self.sort == column {
                        self.ascending = !self.ascending;
                    } else {
                        self.sort = column;
                        self.ascending = true;
                    }
                    let keep = self.selected_row().map(|row| row.path.clone());
                    self.refilter();
                    if let Some(at) = keep.and_then(|path| self.shown.iter().position(|index| self.rows[*index].path == path)) {
                        self.select(at);
                    }
                    break;
                }
            }
            return Outcome::Pending;
        }
        if lay.rows.contains(x, y) {
            self.focus = Focus::List;
            let hit = (0..self.shown.len()).skip(self.top * per_line).take(per_line * (lines + 1)).find(|at| lay.item_rect(ctx, self, *at).is_some_and(|rect| rect.contains(x, y)));
            let Some(at) = hit else {
                self.selected = None;
                return Outcome::Pending;
            };
            let double = matches!(self.last_click, Some((Target::Row(prev), when)) if prev == at && now.saturating_sub(when) <= DOUBLE_CLICK_MS);
            self.last_click = Some((Target::Row(at), now));
            self.select(at);
            self.take_name_from_selection();
            if double {
                self.last_click = None;
                return self.activate(host);
            }
            return Outcome::Pending;
        }
        Outcome::Pending
    }

    /// Сколько элементов в линии и сколько линий видно.
    fn page(&self) -> (usize, usize) {
        match self.geometry.get() {
            Some((ctx, area)) => Layout::new(ctx, area, self).page(ctx, self.view),
            None => (1, 10),
        }
    }

    // -----------------------------------------------------------------------
    // Отрисовка
    // -----------------------------------------------------------------------

    /// Нарисовать окно поверх `area` (окно программы затемняется).
    pub fn draw(&self, host: &dyn Host, s: &mut Surface, ctx: Ctx, area: Rect) {
        self.geometry.set(Some((ctx, area)));
        let lay = Layout::new(ctx, area, self);
        draw::blend_rect(s, area, Color::rgb(0, 0, 0), 110);
        let inner = kit::card(ctx, s, lay.card);
        let p = inner.palette;
        let pad = ctx.px(16) as i32;

        // ── Заголовок и панель ──────────────────────────────────────────────
        let g = ctx.px(18);
        iconset::draw(s, if self.mode == Mode::Save { Fi::Download } else { Fi::FolderOpen }, lay.card.x + pad, lay.close.y + (lay.close.h as i32 - g as i32) / 2, g, p.acc, 255);
        paint::text(inner, s, Role::Title, lay.card.x + pad + g as i32 + ctx.px(10) as i32, paint::baseline(inner, Role::Title, lay.close), &self.title, p.ink);
        ghost_button(inner, s, lay.new_folder, Some(Fi::Plus), "Новая папка");
        segmented(inner, s, lay.view_details, Fi::List, self.view == View::Details);
        segmented(inner, s, lay.view_icons, Fi::Apps, self.view == View::Icons);
        icon_only(inner, s, lay.close, Fi::X, true);

        // ── Строка адреса ───────────────────────────────────────────────────
        icon_only(inner, s, lay.back, Fi::ArrowLeft, !self.back.is_empty());
        icon_only(inner, s, lay.forward, Fi::ArrowRight, !self.forward.is_empty());
        icon_only(inner, s, lay.up, Fi::ArrowUp, self.dir != COMPUTER);
        paint::sunk(inner, s, lay.address, ctx.px(theme::R_ROW));
        let field_ctx = inner.on(inner.flat(p.sunk));
        if let Some(line) = &self.address {
            draw::rounded_stroke(s, lay.address, ctx.px(theme::R_ROW), p.acc, 255);
            line.draw(field_ctx, s, lay.address_text(ctx), true, p.ink);
        } else {
            let gi = ctx.px(16);
            let place_icon = if self.dir == COMPUTER { Fi::Laptop } else { Fi::Folder };
            iconset::draw(s, place_icon, lay.address.x + ctx.px(10) as i32, lay.address.y + (lay.address.h as i32 - gi as i32) / 2, gi, p.warn, 255);
            let crumbs = self.crumbs(ctx, &lay);
            if crumbs.len() < self.crumb_paths().len() {
                paint::text(field_ctx, s, Role::Body, lay.address.x + ctx.px(32) as i32, paint::baseline(ctx, Role::Body, lay.address), "…", p.ink3);
            }
            for (rect, path) in &crumbs {
                let label = self.crumb_label(path);
                let gs = ctx.px(10);
                iconset::draw(s, Fi::ChevronRight, rect.x - ctx.px(14) as i32, lay.address.y + (lay.address.h as i32 - gs as i32) / 2, gs, p.ink4, 255);
                let last = *path == self.dir;
                paint::text_centered(field_ctx, s, Role::Body, *rect, &label, if last { p.ink } else { p.ink2 });
            }
        }
        icon_only(field_ctx, s, lay.refresh, Fi::Refresh, true);
        paint::sunk(inner, s, lay.search, ctx.px(theme::R_ROW));
        if self.focus == Focus::Search {
            draw::rounded_stroke(s, lay.search, ctx.px(theme::R_ROW), p.acc, 255);
        }
        let gs = ctx.px(14);
        let search_text = lay.search_text(ctx);
        iconset::draw(s, Fi::Search, search_text.right() + ctx.px(6) as i32, lay.search.y + (lay.search.h as i32 - gs as i32) / 2, gs, p.ink4, 255);
        if self.search.text.is_empty() && self.focus != Focus::Search {
            let hint = if self.dir == COMPUTER { String::from("Поиск") } else { format!("Поиск: {}", self.crumb_label(&self.dir)) };
            paint::text_clipped(field_ctx, s, Role::Body, search_text.x, paint::baseline(ctx, Role::Body, search_text), search_text.w, &hint, p.ink5);
        } else {
            self.search.draw(field_ctx, s, search_text, self.focus == Focus::Search, p.ink);
        }

        // ── Тело: быстрый доступ и список ───────────────────────────────────
        paint::sunk(inner, s, lay.body, ctx.px(theme::R_CARD));
        let well = inner.on(inner.flat(p.sunk));
        if !lay.places.is_empty() {
            draw::vline(s, lay.places.right(), lay.body.y + ctx.px(8) as i32, lay.body.h.saturating_sub(ctx.px(16)), p.line.color, p.line.alpha);
            for (index, (place, rect)) in self.places.iter().zip(lay.place_rects(ctx, &self.places)).enumerate() {
                if rect.is_empty() {
                    continue;
                }
                if place.gap {
                    paint::separator(well, s, rect.x + ctx.px(8) as i32, rect.y - ctx.px(5) as i32, rect.w.saturating_sub(ctx.px(16)));
                }
                let current = place.path == self.dir;
                let cursor = self.focus == Focus::Places && index == self.place_cursor;
                if current {
                    paint::row(well, s, rect, RowState::Selected);
                } else if cursor {
                    paint::row(well, s, rect, RowState::Hover);
                }
                if cursor {
                    draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.acc, 255);
                }
                let side = ctx.px(18);
                let icon_rect = Rect::new(rect.x + ctx.px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
                if !host.draw_icon(s, icon_rect, place.icon) {
                    iconset::draw(s, place.fi, icon_rect.x, icon_rect.y, side, p.acc_ink, 255);
                }
                let tx = icon_rect.right() + ctx.px(8) as i32;
                paint::text_clipped(well, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), (rect.right() - tx - ctx.px(6) as i32).max(0) as u32, &place.label, if current { p.ink } else { p.ink2 });
            }
        }
        self.draw_list(host, well, s, &lay);

        // ── Низ: имя, тип, кнопки ───────────────────────────────────────────
        let label_role = Role::Body;
        paint::text_right(inner, s, label_role, lay.name.x - ctx.px(10) as i32, paint::baseline(ctx, label_role, lay.name), "Имя файла:", p.ink2);
        paint::text_right(inner, s, label_role, lay.kind.x - ctx.px(10) as i32, paint::baseline(ctx, label_role, lay.kind), "Тип файла:", p.ink2);
        paint::sunk(inner, s, lay.name, ctx.px(theme::R_ROW));
        if self.focus == Focus::Name {
            draw::rounded_stroke(s, lay.name, ctx.px(theme::R_ROW), p.acc, 255);
        }
        self.name.draw(field_ctx, s, field_text(ctx, lay.name), self.focus == Focus::Name, p.ink);
        paint::sunk(inner, s, lay.kind, ctx.px(theme::R_ROW));
        if self.type_menu.is_some() {
            draw::rounded_stroke(s, lay.kind, ctx.px(theme::R_ROW), p.acc, 255);
        }
        let gc = ctx.px(12);
        iconset::draw(s, Fi::ChevronDown, lay.kind.right() - ctx.px(22) as i32, lay.kind.y + (lay.kind.h as i32 - gc as i32) / 2, gc, p.ink3, 255);
        let kind_text = field_text(ctx, lay.kind);
        paint::text_clipped(field_ctx, s, Role::Body, kind_text.x, paint::baseline(ctx, Role::Body, kind_text), kind_text.w.saturating_sub(ctx.px(24)), &self.filters[self.filter].label, p.ink);

        if self.mode == Mode::Save {
            checkbox(inner, s, lay.check, self.add_ext, "Добавлять расширение");
        }
        paint::button(inner, s, lay.ok, Weight::Primary, if self.mode == Mode::Save { "Сохранить" } else { "Открыть" }, false);
        paint::button(inner, s, lay.cancel, Weight::Normal, "Отмена", false);
        let (message, ink) = match &self.error {
            Some(error) => (error.clone(), p.bad_ink),
            None => (self.status_text(), p.ink4),
        };
        let room = (lay.message_right - lay.card.x - pad).max(0) as u32;
        let y = paint::baseline(ctx, Role::Caption, lay.ok);
        if self.error.is_some() {
            let gw = ctx.px(14);
            iconset::draw(s, Fi::Warning, lay.card.x + pad, lay.ok.y + (lay.ok.h as i32 - gw as i32) / 2, gw, p.bad_ink, 255);
            paint::text_clipped(inner, s, Role::Caption, lay.card.x + pad + gw as i32 + ctx.px(6) as i32, y, room.saturating_sub(gw + ctx.px(6)), &message, ink);
        } else {
            paint::text_clipped(inner, s, Role::Caption, lay.card.x + pad, y, room, &message, ink);
        }

        // ── Поверх: список типов, вопрос о замене ───────────────────────────
        if let Some(cursor) = self.type_menu {
            let (rect, entries) = self.type_menu_layout(ctx, &lay, area);
            kit::draw_menu(ctx, s, rect, &entries, cursor);
        }
        if let Some(path) = &self.confirm {
            let (card, yes, no) = confirm_layout(ctx, lay.card);
            draw::blend_rect(s, lay.card, Color::rgb(0, 0, 0), 70);
            let inner = kit::card(ctx, s, card);
            let gw = ctx.px(22);
            iconset::draw(s, Fi::Warning, card.x + pad, card.y + pad, gw, p.warn, 255);
            let tx = card.x + pad + gw as i32 + ctx.px(12) as i32;
            paint::text(inner, s, Role::Title, tx, card.y + pad, "Подтвердить сохранение", p.ink);
            let leaf = path.rsplit('/').next().unwrap_or(path);
            let room = (card.right() - tx - pad).max(0) as u32;
            paint::text_clipped(inner, s, Role::Body, tx, card.y + pad + ctx.px(32) as i32, room, &format!("«{leaf}» уже существует."), p.ink2);
            paint::text_clipped(inner, s, Role::Body, tx, card.y + pad + ctx.px(54) as i32, room, "Заменить его?", p.ink2);
            paint::button(inner, s, yes, Weight::Primary, "Да", false);
            paint::button(inner, s, no, Weight::Normal, "Нет", false);
        }
    }

    fn draw_list(&self, host: &dyn Host, ctx: Ctx, s: &mut Surface, lay: &Layout) {
        let p = ctx.palette;
        let (per_line, lines) = lay.page(ctx, self.view);
        if self.view == View::Details {
            let columns = lay.columns(ctx);
            let titles = ["Имя", "Дата изменения", "Тип", "Размер"];
            for (index, (rect, title)) in columns.iter().zip(titles).enumerate() {
                if rect.w == 0 {
                    continue;
                }
                let column = [Column::Name, Column::Date, Column::Kind, Column::Size][index];
                let y = paint::baseline(ctx, Role::Caption, lay.header);
                let tw = paint::text(ctx, s, Role::Caption, rect.x + ctx.px(8) as i32, y, title, if self.sort == column { p.ink2 } else { p.ink4 });
                if self.sort == column {
                    let gc = ctx.px(10);
                    iconset::draw(s, if self.ascending { Fi::ChevronUp } else { Fi::ChevronDown }, rect.x + ctx.px(14) as i32 + tw as i32, lay.header.y + (lay.header.h as i32 - gc as i32) / 2, gc, p.ink3, 255);
                }
                if index > 0 {
                    draw::vline(s, rect.x, lay.header.y + ctx.px(7) as i32, lay.header.h.saturating_sub(ctx.px(14)), p.line.color, p.line.alpha);
                }
            }
            draw::hline(s, lay.header.x + ctx.px(6) as i32, lay.header.bottom() - 1, lay.header.w.saturating_sub(ctx.px(12)), p.line.color, p.line.alpha);
        }

        if self.shown.is_empty() {
            let text = if !self.search.text.is_empty() {
                "Ничего не найдено"
            } else if self.error.is_some() {
                ""
            } else {
                "Эта папка пуста"
            };
            let rect = Rect::new(lay.rows.x, lay.rows.y + ctx.px(18) as i32, lay.rows.w, ctx.px(24));
            paint::text_centered(ctx, s, Role::Body, rect, text, p.ink4);
        }

        let columns = lay.columns(ctx);
        let first = self.top * per_line;
        for at in first..(first + per_line * lines).min(self.shown.len()) {
            let Some(rect) = lay.item_rect(ctx, self, at) else {
                continue;
            };
            let index = self.shown[at];
            let row = &self.rows[index];
            let selected = self.selected == Some(at);
            if selected {
                paint::row(ctx, s, rect, RowState::Selected);
                if self.focus == Focus::List {
                    draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.acc, 255);
                }
            }
            let renaming = self.rename.as_ref().filter(|(rename, _)| *rename == index).map(|(_, line)| line);
            if self.view == View::Details {
                let side = ctx.px(18);
                let icon = Rect::new(columns[0].x + ctx.px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
                self.draw_item_icon(host, s, ctx, icon, row);
                let tx = icon.right() + ctx.px(8) as i32;
                let name_room = (columns[0].right() - tx - ctx.px(8) as i32).max(0) as u32;
                if let Some(line) = renaming {
                    let field = Rect::new(tx - ctx.px(4) as i32, rect.y + ctx.px(2) as i32, name_room + ctx.px(4), rect.h.saturating_sub(ctx.px(4)));
                    draw::rounded(s, field, ctx.px(3), ctx.flat(p.panel), 255);
                    draw::rounded_stroke(s, field, ctx.px(3), p.acc, 255);
                    line.draw(ctx, s, Rect::new(tx, field.y, name_room, field.h), true, p.ink);
                } else {
                    paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), name_room, &row.label, if selected { p.ink } else { p.ink2 });
                }
                let cells = [String::new(), row.date.clone(), row.kind.clone(), if (row.dir && !row.volume) || (row.volume && row.size == 0) { String::new() } else { human_size(row.size) }];
                for (column, text) in columns.iter().zip(cells.iter()).skip(1) {
                    if column.w == 0 || text.is_empty() {
                        continue;
                    }
                    paint::text_clipped(ctx, s, Role::Body, column.x + ctx.px(8) as i32, paint::baseline(ctx, Role::Body, rect), column.w.saturating_sub(ctx.px(14)), text, p.ink4);
                }
            } else {
                let side = ctx.px(48);
                let icon = Rect::new(rect.x + (rect.w as i32 - side as i32) / 2, rect.y + ctx.px(8) as i32, side, side);
                self.draw_item_icon(host, s, ctx, icon, row);
                let text_rect = Rect::new(rect.x + ctx.px(4) as i32, icon.bottom() + ctx.px(6) as i32, rect.w.saturating_sub(ctx.px(8)), u32::from(ctx.face(Role::Caption).line));
                if let Some(line) = renaming {
                    let field = Rect::new(rect.x + ctx.px(2) as i32, text_rect.y - ctx.px(3) as i32, rect.w.saturating_sub(ctx.px(4)), text_rect.h + ctx.px(6));
                    draw::rounded(s, field, ctx.px(3), ctx.flat(p.panel), 255);
                    draw::rounded_stroke(s, field, ctx.px(3), p.acc, 255);
                    line.draw(ctx, s, field.shrink(ctx.px(3)), true, p.ink);
                } else {
                    let face = ctx.face(Role::Caption);
                    let w = face.width(&row.label).min(text_rect.w);
                    let x = text_rect.x + (text_rect.w as i32 - w as i32) / 2;
                    paint::text_clipped(ctx, s, Role::Caption, x, text_rect.y, text_rect.w, &row.label, if selected { p.ink } else { p.ink2 });
                }
            }
        }

        // Полоса прокрутки (TODO: перейти на kit::ScrollBar, когда он появится в kit.rs).
        let total = self.shown.len().div_ceil(per_line);
        if let Some((track, thumb)) = scrollbar(lay, total, lines, self.top) {
            draw::rounded(s, track, track.w / 2, p.hover1.color, p.hover1.alpha);
            let ink = if self.grab.is_some() { p.ink3 } else { p.ink5 };
            draw::rounded(s, thumb, thumb.w / 2, ink, 255);
        }
    }

    fn draw_item_icon(&self, host: &dyn Host, s: &mut Surface, ctx: Ctx, rect: Rect, row: &Row) {
        if host.draw_icon(s, rect, row.icon) {
            return;
        }
        let p = ctx.palette;
        let (fi, ink) = if row.volume {
            (if row.icon == "place-usb-stick" { Fi::Usb } else { Fi::Drive }, p.acc_ink)
        } else if row.dir {
            (Fi::Folder, p.warn)
        } else if row.icon == "doc-text" {
            (Fi::FileText, p.ink3)
        } else if row.icon == "doc-code" {
            (Fi::FileCode, p.ink3)
        } else if row.icon == "doc-image" {
            (Fi::Image, p.ink3)
        } else {
            (Fi::File, p.ink3)
        };
        iconset::draw(s, fi, rect.x, rect.y, rect.w, ink, 255);
    }

    /// «Элементов: 12 | Выбран: заметка.txt, 2 КБ».
    fn status_text(&self) -> String {
        let mut text = format!("Элементов: {}", self.shown.len());
        if let Some(row) = self.selected_row() {
            if row.volume && row.size > 0 {
                text.push_str(&format!("   ·   «{}»: свободно {} из {}", row.label, human_size(row.free), human_size(row.size)));
            } else if row.dir {
                text.push_str(&format!("   ·   Выбрана папка «{}»", row.label));
            } else {
                text.push_str(&format!("   ·   Выбран «{}», {}", row.label, human_size(row.size)));
            }
        }
        text
    }

    fn type_menu_layout(&self, ctx: Ctx, lay: &Layout, area: Rect) -> (Rect, Vec<kit::Entry<'_>>) {
        let entries: Vec<kit::Entry<'_>> = self.filters.iter().map(|filter| kit::Entry { title: &filter.label, icon: None, danger: false, group: 0 }).collect();
        let (w, h) = kit::menu_size(ctx, &entries);
        let w = w.max(lay.kind.w);
        let below = lay.kind.bottom() + ctx.px(4) as i32;
        let y = if below + h as i32 <= area.bottom() { below } else { lay.kind.y - ctx.px(4) as i32 - h as i32 };
        (kit::place((lay.kind.x, y), (w, h), area), entries)
    }

    // ── Адрес ───────────────────────────────────────────────────────────────

    /// Пути звеньев адреса от корня.
    fn crumb_paths(&self) -> Vec<String> {
        if self.dir == COMPUTER {
            return vec![String::from(COMPUTER)];
        }
        let mut out = Vec::new();
        let (mut current, rest) = if self.home != "/" && (self.dir == self.home || self.dir.starts_with(&format!("{}/", self.home))) {
            out.push(self.home.clone());
            (self.home.clone(), self.dir[self.home.len()..].to_string())
        } else {
            out.push(String::from(COMPUTER));
            out.push(String::from("/"));
            (String::new(), self.dir.clone())
        };
        for part in rest.split('/').filter(|part| !part.is_empty()) {
            current = format!("{current}/{part}");
            out.push(current.clone());
        }
        out
    }

    fn crumb_label(&self, path: &str) -> String {
        if path == COMPUTER {
            return String::from("Этот компьютер");
        }
        if path == "/" {
            return String::from("Системный диск");
        }
        if path == self.home {
            return String::from("Главная");
        }
        let leaf = path.rsplit('/').next().unwrap_or(path);
        display_name(&parent_of(path), &self.home, leaf)
    }

    /// Звенья, которые помещаются: прямоугольник подписи и путь. Не влезает —
    /// теряются первые (на их месте «…»).
    fn crumbs(&self, ctx: Ctx, lay: &Layout) -> Vec<(Rect, String)> {
        let face = ctx.face(Role::Body);
        let paths = self.crumb_paths();
        let start = lay.address.x + ctx.px(32) as i32;
        let end = lay.refresh.x - ctx.px(4) as i32;
        let gap = ctx.px(18) as i32;
        let widths: Vec<i32> = paths.iter().map(|path| face.width(&self.crumb_label(path)) as i32 + ctx.px(8) as i32).collect();
        let mut first = 0;
        let ellipsis = face.width("…") as i32 + ctx.px(4) as i32;
        loop {
            let lead = if first > 0 { ellipsis } else { 0 };
            let total: i32 = lead + widths[first..].iter().map(|w| w + gap).sum::<i32>();
            if start + total <= end || first + 1 >= paths.len() {
                break;
            }
            first += 1;
        }
        let mut x = start + if first > 0 { ellipsis } else { 0 };
        let mut out = Vec::new();
        for (path, w) in paths.iter().zip(&widths).skip(first) {
            x += gap;
            let w = (*w).min((end - x).max(0));
            out.push((Rect::new(x, lay.address.y + ctx.px(3) as i32, w as u32, lay.address.h.saturating_sub(ctx.px(6))), path.clone()));
            x += w;
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Layout {
    card: Rect,
    close: Rect,
    new_folder: Rect,
    view_details: Rect,
    view_icons: Rect,
    back: Rect,
    forward: Rect,
    up: Rect,
    address: Rect,
    refresh: Rect,
    search: Rect,
    body: Rect,
    places: Rect,
    header: Rect,
    rows: Rect,
    scroll: Rect,
    name: Rect,
    kind: Rect,
    check: Rect,
    ok: Rect,
    cancel: Rect,
    /// Где кончается место под сообщение слева от кнопок.
    message_right: i32,
}

impl Layout {
    fn new(ctx: Ctx, area: Rect, dialog: &FileDialog) -> Self {
        let margin = ctx.px(12);
        let w = ctx.px(860).min(area.w.saturating_sub(margin * 2)).max(ctx.px(320).min(area.w));
        let h = ctx.px(560).min(area.h.saturating_sub(margin * 2)).max(ctx.px(300).min(area.h));
        let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
        let pad = ctx.px(16) as i32;
        let gap = ctx.px(6) as i32;
        let b = ctx.px(32);

        // Заголовок: подпись слева; «Новая папка», вид, крестик справа.
        let title_y = card.y + ctx.px(10) as i32;
        let close = Rect::new(card.right() - pad - b as i32, title_y, b, b);
        let view_icons = Rect::new(close.x - ctx.px(14) as i32 - b as i32, title_y, b, b);
        let view_details = Rect::new(view_icons.x - b as i32, title_y, b, b);
        let nf_w = ctx.face(Role::Body).width("Новая папка") + ctx.px(46);
        let new_folder = Rect::new(view_details.x - ctx.px(10) as i32 - nf_w as i32, title_y, nf_w, b);

        // Адрес: назад, вперёд, вверх, крошки с «обновить», поиск.
        let nav_y = close.bottom() + ctx.px(10) as i32;
        let back = Rect::new(card.x + pad, nav_y, b, b);
        let forward = Rect::new(back.right() + gap / 2, nav_y, b, b);
        let up = Rect::new(forward.right() + gap / 2, nav_y, b, b);
        let inner_w = (card.right() - pad - (up.right() + gap * 2)).max(0) as u32;
        let search_w = (inner_w / 4).clamp(ctx.px(120).min(inner_w / 2), ctx.px(240));
        let search = Rect::new(card.right() - pad - search_w as i32, nav_y, search_w, b);
        let address = Rect::new(up.right() + gap * 2, nav_y, (search.x - gap - up.right() - gap * 2).max(0) as u32, b);
        let refresh = Rect::new(address.right() - b as i32, nav_y, b, b);

        // Низ: имя, тип, кнопки — снизу вверх.
        let field_h = ctx.px(32);
        let button_h = ctx.px(34);
        let bw = ctx.px(116);
        let cancel = Rect::new(card.right() - pad - bw as i32, card.bottom() - pad - button_h as i32, bw, button_h);
        let ok = Rect::new(cancel.x - ctx.px(10) as i32 - bw as i32, cancel.y, bw, button_h);
        let check_label = ctx.face(Role::Body).width("Добавлять расширение");
        let check_w = check_label + ctx.px(28);
        let check = if dialog.mode == Mode::Save { Rect::new(ok.x - ctx.px(24) as i32 - check_w as i32, ok.y, check_w, button_h) } else { Rect::EMPTY };
        let message_right = if dialog.mode == Mode::Save { check.x - ctx.px(12) as i32 } else { ok.x - ctx.px(12) as i32 };
        let label_w = ctx.face(Role::Body).width("Имя файла:").max(ctx.face(Role::Body).width("Тип файла:")) + ctx.px(20);
        let fields_x = card.x + pad + label_w as i32;
        let fields_w = (card.right() - pad - fields_x).max(0) as u32;
        let kind = Rect::new(fields_x, ok.y - ctx.px(14) as i32 - field_h as i32, fields_w, field_h);
        let name = Rect::new(fields_x, kind.y - ctx.px(8) as i32 - field_h as i32, fields_w, field_h);

        // Тело.
        let body_y = nav_y + b as i32 + ctx.px(10) as i32;
        let body = Rect::new(card.x + pad, body_y, (card.w as i32 - pad * 2).max(0) as u32, (name.y - ctx.px(14) as i32 - body_y).max(0) as u32);
        let places_w = if body.w >= ctx.px(560) { ctx.px(200).min(body.w / 3) } else { 0 };
        let places = Rect::new(body.x, body.y, places_w, body.h);
        let list_x = body.x + places_w as i32 + if places_w > 0 { 1 } else { 0 };
        let list_w = (body.right() - list_x).max(0) as u32;
        let header_h = if dialog.view == View::Details { ctx.px(30) } else { 0 };
        let sb = ctx.px(8);
        let header = Rect::new(list_x, body.y + ctx.px(2) as i32, list_w.saturating_sub(sb + ctx.px(8)), header_h);
        let rows_y = header.bottom() + ctx.px(4) as i32;
        let rows = Rect::new(list_x, rows_y, list_w.saturating_sub(sb + ctx.px(8)), (body.bottom() - ctx.px(4) as i32 - rows_y).max(0) as u32);
        let scroll = Rect::new(body.right() - ctx.px(4) as i32 - sb as i32, rows_y, sb, rows.h);

        Self {
            card,
            close,
            new_folder,
            view_details,
            view_icons,
            back,
            forward,
            up,
            address,
            refresh,
            search,
            body,
            places,
            header,
            rows,
            scroll,
            name,
            kind,
            check,
            ok,
            cancel,
            message_right,
        }
    }

    fn address_text(&self, ctx: Ctx) -> Rect {
        Rect::new(self.address.x + ctx.px(10) as i32, self.address.y, (self.refresh.x - self.address.x - ctx.px(14) as i32).max(0) as u32, self.address.h)
    }

    fn search_text(&self, ctx: Ctx) -> Rect {
        Rect::new(self.search.x + ctx.px(10) as i32, self.search.y, self.search.w.saturating_sub(ctx.px(36)), self.search.h)
    }

    fn row_h(ctx: Ctx) -> u32 {
        ctx.px(28)
    }

    fn cell(ctx: Ctx) -> (u32, u32) {
        (ctx.px(104), ctx.px(96))
    }

    /// Сколько элементов в линии и сколько линий помещается.
    fn page(&self, ctx: Ctx, view: View) -> (usize, usize) {
        match view {
            View::Details => (1, (self.rows.h / Self::row_h(ctx).max(1)).max(1) as usize),
            View::Icons => {
                let (cw, ch) = Self::cell(ctx);
                (((self.rows.w.saturating_sub(ctx.px(8))) / cw.max(1)).max(1) as usize, (self.rows.h / ch.max(1)).max(1) as usize)
            }
        }
    }

    /// Прямоугольник элемента номер `at` (в `shown`), если он виден.
    fn item_rect(&self, ctx: Ctx, dialog: &FileDialog, at: usize) -> Option<Rect> {
        let (per_line, lines) = self.page(ctx, dialog.view);
        let line = at / per_line;
        if line < dialog.top || line >= dialog.top + lines {
            return None;
        }
        let offset = (line - dialog.top) as i32;
        Some(match dialog.view {
            View::Details => {
                let h = Self::row_h(ctx);
                Rect::new(self.rows.x + ctx.px(6) as i32, self.rows.y + offset * h as i32, self.rows.w.saturating_sub(ctx.px(8)), h.saturating_sub(ctx.px(2)))
            }
            View::Icons => {
                let (cw, ch) = Self::cell(ctx);
                let column = (at % per_line) as i32;
                Rect::new(self.rows.x + ctx.px(8) as i32 + column * cw as i32, self.rows.y + offset * ch as i32, cw.saturating_sub(ctx.px(4)), ch.saturating_sub(ctx.px(4)))
            }
        })
    }

    /// Столбцы таблицы: имя тянется, остальные теряются справа налево, когда
    /// имени остаётся меньше 180 точек.
    fn columns(&self, ctx: Ctx) -> [Rect; 4] {
        let left = self.rows.x + ctx.px(6) as i32;
        let right = self.rows.right() - ctx.px(2) as i32;
        let widths = [ctx.px(140), ctx.px(150), ctx.px(90)];
        let mut shown = 0;
        let mut used = 0u32;
        for width in widths {
            if (right - left) - (used + width) as i32 >= ctx.px(180) as i32 {
                used += width;
                shown += 1;
            } else {
                break;
            }
        }
        let mut out = [Rect::EMPTY; 4];
        let name_w = (right - left - used as i32).max(0) as u32;
        out[0] = Rect::new(left, self.header.y, name_w, self.header.h);
        let mut x = left + name_w as i32;
        for (index, width) in widths.iter().enumerate().take(shown) {
            out[index + 1] = Rect::new(x, self.header.y, *width, self.header.h);
            x += *width as i32;
        }
        out
    }

    /// Строки быстрого доступа; не влезшие — пустые.
    fn place_rects(&self, ctx: Ctx, places: &[Place]) -> Vec<Rect> {
        let h = ctx.px(30);
        let mut y = self.places.y + ctx.px(6) as i32;
        let mut out = Vec::with_capacity(places.len());
        for place in places {
            if place.gap {
                y += ctx.px(11) as i32;
            }
            let indent = if place.indent { ctx.px(16) } else { 0 };
            let rect = Rect::new(self.places.x + ctx.px(6) as i32 + indent as i32, y, self.places.w.saturating_sub(ctx.px(12) + indent), h - ctx.px(2));
            out.push(if rect.bottom() <= self.places.bottom() - ctx.px(4) as i32 && !self.places.is_empty() { rect } else { Rect::EMPTY });
            y += h as i32;
        }
        out
    }
}

/// Дорожка и ползунок полосы прокрутки, если всё не помещается.
fn scrollbar(lay: &Layout, total: usize, visible: usize, top: usize) -> Option<(Rect, Rect)> {
    if total <= visible || lay.scroll.h == 0 {
        return None;
    }
    let track = lay.scroll;
    let min = (track.w * 3).min(track.h);
    let thumb_h = ((u64::from(track.h) * visible as u64 / total as u64) as u32).clamp(min, track.h);
    let last = total - visible;
    let y = track.y + ((u64::from(track.h - thumb_h) * top.min(last) as u64) / last.max(1) as u64) as i32;
    Some((track, Rect::new(track.x, y, track.w, thumb_h)))
}

/// Карточка «Заменить?» и её кнопки «Да», «Нет».
fn confirm_layout(ctx: Ctx, over: Rect) -> (Rect, Rect, Rect) {
    let w = ctx.px(440).min(over.w.saturating_sub(ctx.px(24)));
    let h = ctx.px(150);
    let card = Rect::new(over.x + (over.w as i32 - w as i32) / 2, over.y + (over.h as i32 - h as i32) / 2, w, h);
    let pad = ctx.px(16) as i32;
    let bw = ctx.px(100);
    let by = card.bottom() - pad - ctx.px(32) as i32;
    let no = Rect::new(card.right() - pad - bw as i32, by, bw, ctx.px(32));
    let yes = Rect::new(no.x - ctx.px(10) as i32 - bw as i32, by, bw, ctx.px(32));
    (card, yes, no)
}

fn field_text(ctx: Ctx, field: Rect) -> Rect {
    Rect::new(field.x + ctx.px(10) as i32, field.y, field.w.saturating_sub(ctx.px(20)), field.h)
}

// ── Кнопки ──────────────────────────────────────────────────────────────────

fn icon_only(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, enabled: bool) {
    let p = ctx.palette;
    let side = rect.w.min(rect.h) / 2;
    let ink = if enabled { p.ink2 } else { p.ink6 };
    iconset::draw(s, icon, rect.x + (rect.w as i32 - side as i32) / 2, rect.y + (rect.h as i32 - side as i32) / 2, side, ink, 255);
}

fn ghost_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Option<Fi>, label: &str) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_ROW);
    draw::rounded(s, rect, r, ctx.flat(p.btn), 255);
    draw::rounded_stroke(s, rect, r, p.btnline.color, p.btnline.alpha);
    let mut x = rect.x + ctx.px(12) as i32;
    if let Some(icon) = icon {
        let g = ctx.px(14);
        iconset::draw(s, icon, x, rect.y + (rect.h as i32 - g as i32) / 2, g, p.acc_ink, 255);
        x += g as i32 + ctx.px(8) as i32;
    }
    paint::text(ctx, s, Role::Body, x, paint::baseline(ctx, Role::Body, rect), label, p.ink2);
}

fn segmented(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, on: bool) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_ROW);
    if on {
        draw::rounded(s, rect, r, ctx.flat(p.btn), 255);
        draw::rounded_stroke(s, rect, r, p.btnline.color, p.btnline.alpha);
    }
    let side = rect.w.min(rect.h) * 7 / 16;
    iconset::draw(s, icon, rect.x + (rect.w as i32 - side as i32) / 2, rect.y + (rect.h as i32 - side as i32) / 2, side, if on { p.ink } else { p.ink4 }, 255);
}

fn checkbox(ctx: Ctx, s: &mut Surface, rect: Rect, on: bool, label: &str) {
    let p = ctx.palette;
    let side = ctx.px(18);
    let square = Rect::new(rect.x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
    if on {
        draw::rounded(s, square, ctx.px(4), p.acc, 255);
        let g = ctx.px(12);
        iconset::draw(s, Fi::Check, square.x + (side as i32 - g as i32) / 2, square.y + (side as i32 - g as i32) / 2, g, Color::rgb(0xFF, 0xFF, 0xFF), 255);
    } else {
        draw::rounded(s, square, ctx.px(4), ctx.flat(p.sunk), 255);
        draw::rounded_stroke(s, square, ctx.px(4), p.line3.color, p.line3.alpha);
    }
    paint::text(ctx, s, Role::Body, square.right() + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink2);
}

// ---------------------------------------------------------------------------
// Пути и слова
// ---------------------------------------------------------------------------

/// Имя в папке: `dir` + `/` + `name`.
#[must_use]
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// Папка выше. У корня — сам корень.
#[must_use]
pub fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) | None => String::from("/"),
        Some(at) => path[..at].to_string(),
    }
}

/// Убрать `.`, `..` и двойные черты.
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
    let mut out = String::new();
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() { String::from("/") } else { out }
}

/// Расширение строчными без точки (`.bashrc` — без расширения).
fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Подпись записи: папки пользователя в доме — по-русски, остальное как есть.
fn display_name(dir: &str, home: &str, name: &str) -> String {
    if dir == home {
        if let Some((_, label, ..)) = STANDARD.iter().find(|(folder, ..)| *folder == name) {
            return label.to_string();
        }
    }
    name.to_string()
}

/// Размер по-человечески: «48 КБ», «1,2 МБ».
#[must_use]
pub fn human_size(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes < K {
        format!("{bytes} Б")
    } else if bytes < K * K {
        format!("{} КБ", bytes.div_ceil(K))
    } else if bytes < K * K * K {
        let tenths = bytes * 10 / (K * K);
        if tenths < 100 { format!("{},{} МБ", tenths / 10, tenths % 10) } else { format!("{} МБ", tenths / 10) }
    } else {
        let tenths = bytes * 10 / (K * K * K);
        format!("{},{} ГБ", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(normalize("/root//Desktop/./a/../b.txt"), "/root/Desktop/b.txt");
        assert_eq!(parent_of("/root/Desktop"), "/root");
        assert_eq!(parent_of("/root"), "/");
        assert_eq!(join("/", "usr"), "/usr");
        assert_eq!(extension("note.TXT"), "txt");
        assert_eq!(extension(".bashrc"), "");
    }

    #[test]
    fn line_editing() {
        let mut line = Line::selected("Без имени.txt");
        assert_eq!(line.key('a' as u32, 0, 'a' as u32), Some(true));
        assert_eq!(line.text, "a");
        line.key(BACKSPACE, 0, 0);
        line.key(BACKSPACE, 0, 0);
        assert_eq!(line.text, "");
        for ch in "копия.txt".chars() {
            line.key(ch as u32, 0, 0);
        }
        line.key(KEY_HOME, MOD_SHIFT, 0);
        line.key(KEY_DELETE, 0, 0);
        assert!(line.text.is_empty());
    }

    #[test]
    fn filters() {
        let text = Filter::new("Текстовые файлы", &["txt"]);
        assert_eq!(text.label, "Текстовые файлы (*.txt)");
        assert!(text.matches("a.TXT") && !text.matches("a.rs"));
        assert!(Filter::all().matches("anything"));
    }
}
