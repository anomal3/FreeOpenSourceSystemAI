//! Блокнот: свой текстовый редактор с окном (фаза 70b).
//!
//! # Зачем свой
//!
//! В системе есть `mc` с редактором — но в терминале и с клавишами, которые
//! человек с Windows не знает. Блокнот — первое, что он ищет в «Пуске»: открыть
//! файл, поправить строку, сохранить. Здесь ровно это и ничего сверх.
//!
//! # Что умеет
//!
//! Открыть файл (`/bin/notepad путь` или Ctrl+O), набирать в любой раскладке,
//! ходить стрелками, Home/End, PageUp/PageDown, Backspace/Delete, сохранить
//! (Ctrl+S) и сохранить под другим именем (Ctrl+Shift+S нет — кнопка «Как…»),
//! создать новый (Ctrl+N). Перенос строк выключен: длинная строка уезжает
//! вправо вслед за кареткой. Текст — UTF-8; файл, который не UTF-8, не
//! открывается, а не превращается в мусор при сохранении.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN,
    WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, file_size, open, open_write, println, read, write,
};

const SPEC: Spec = Spec { name: "notepad", title: "Notepad", period_ms: 1_000 };

/// Самый большой файл, который блокнот откроет: четыре мегабайта. Больше —
/// это уже не заметка, и держать его строками в куче программы незачем.
const MAX_FILE: usize = 4 * 1024 * 1024;

const ENTER: u32 = '\n' as u32;
const TAB: u32 = '\t' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const CTRL_N: u32 = 0x0E;
const CTRL_O: u32 = 0x0F;
const CTRL_S: u32 = 0x13;

const TOOLBAR_H: u32 = 46;
const STATUS_H: u32 = 28;
const GUTTER_CHARS: usize = 4;

/// Какое поле ввода открыто поверх текста.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Open,
    SaveAs,
}

struct Prompt {
    ask: Ask,
    text: String,
    error: Option<String>,
}

struct Editor {
    /// Строки без перевода строки на конце.
    lines: Vec<String>,
    /// Каретка: строка и номер **символа** в ней (не байта).
    row: usize,
    col: usize,
    /// Первая показанная строка и первый показанный столбец.
    top: usize,
    left: usize,
    path: Option<String>,
    dirty: bool,
    prompt: Option<Prompt>,
    note: Option<String>,
    /// Сколько строк и столбцов помещается — запоминается при отрисовке, чтобы
    /// PageDown листал ровно на экран.
    page_rows: core::cell::Cell<usize>,
    page_cols: core::cell::Cell<usize>,
}

impl Editor {
    fn new(path: Option<&str>) -> Self {
        let mut editor = Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            top: 0,
            left: 0,
            path: None,
            dirty: false,
            prompt: None,
            note: None,
            page_rows: core::cell::Cell::new(20),
            page_cols: core::cell::Cell::new(80),
        };
        if let Some(path) = path {
            editor.load(path);
        }
        editor
    }

    fn title(&self) -> String {
        let name = self.path.as_deref().map_or("Без имени", |path| path.rsplit('/').next().unwrap_or(path));
        format!("{}{name}", if self.dirty { "● " } else { "" })
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
                self.row = 0;
                self.col = 0;
                self.top = 0;
                self.left = 0;
                self.dirty = false;
                self.note = Some(format!("Открыт {path}: строк {}", self.lines.len()));
                println(&format!("notepad: opened {path}, {} line(s)", self.lines.len()));
            }
            Err(why) => {
                // Несуществующий файл — это новый файл с таким именем: так
                // ведёт себя Блокнот Windows, и `notepad заметка.txt` должно
                // работать и тогда, когда заметки ещё нет.
                if why == "нет такого файла" {
                    self.path = Some(path.to_string());
                    self.note = Some(format!("Новый файл {path}"));
                    println(&format!("notepad: new file {path}"));
                } else {
                    self.note = Some(format!("Не открыть {path}: {why}"));
                    println(&format!("notepad: cannot open {path}: {why}"));
                }
            }
        }
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.prompt = Some(Prompt { ask: Ask::SaveAs, text: String::from("/home/"), error: None });
            return;
        };
        let mut text = self.lines.join("\n");
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let fd = open_write(&path, true, true);
        if fd < 0 {
            self.note = Some(format!("Не сохранить {path}: {}", error_text(fd)));
            println(&format!("notepad: cannot save {path}: {fd}"));
            return;
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
            return;
        }
        self.dirty = false;
        self.note = Some(format!("Сохранено: {path}, {} байт", bytes.len()));
        println(&format!("notepad: saved {path}, {} byte(s)", bytes.len()));
    }

    fn new_file(&mut self) {
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.top = 0;
        self.left = 0;
        self.path = None;
        self.dirty = false;
        self.note = Some(String::from("Новый документ"));
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

    fn insert(&mut self, ch: char) {
        let line = &mut self.lines[self.row];
        let at = Self::byte_at(line, self.col);
        line.insert(at, ch);
        self.col += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
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
    }

    fn backspace(&mut self) {
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
    }

    fn delete(&mut self) {
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
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.lines.len() - 1);
        self.col = col.min(self.line_len(self.row));
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

    fn prompt_key(&mut self, code: u32) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match code {
            ESCAPE => self.prompt = None,
            BACKSPACE => {
                prompt.text.pop();
                prompt.error = None;
            }
            ENTER => {
                let path = prompt.text.trim().to_string();
                if !path.starts_with('/') {
                    prompt.error = Some(String::from("Путь начинается с косой: /home/roman/заметка.txt"));
                    return;
                }
                let ask = prompt.ask;
                self.prompt = None;
                match ask {
                    Ask::Open => self.load(&path),
                    Ask::SaveAs => {
                        self.path = Some(path);
                        self.save();
                    }
                }
            }
            code if code < 0x0100_0000 => {
                if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                    if prompt.text.len() < 200 {
                        prompt.text.push(ch);
                    }
                    prompt.error = None;
                }
            }
            _ => {}
        }
    }

    fn ask_open(&mut self) {
        let start = self.path.as_deref().map_or(String::from("/home/"), |path| {
            let dir = path.rfind('/').map_or("/", |at| &path[..=at]);
            dir.to_string()
        });
        self.prompt = Some(Prompt { ask: Ask::Open, text: start, error: None });
    }

    fn ask_save_as(&mut self) {
        let start = self.path.clone().unwrap_or_else(|| String::from("/home/"));
        self.prompt = Some(Prompt { ask: Ask::SaveAs, text: start, error: None });
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    toolbar: Rect,
    buttons: [Rect; 4],
    text: Rect,
    status: Rect,
}

const BUTTONS: [(Fi, &str); 4] = [(Fi::Plus, "Новый"), (Fi::FolderOpen, "Открыть"), (Fi::Download, "Сохранить"), (Fi::Copy, "Как…")];

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let toolbar = Rect::new(area.x, area.y, area.w, ctx.px(TOOLBAR_H));
        let status = Rect::new(area.x, area.bottom() - ctx.px(STATUS_H) as i32, area.w, ctx.px(STATUS_H));
        let text = Rect::new(area.x, toolbar.bottom(), area.w, (status.y - toolbar.bottom()).max(0) as u32);
        let mut buttons = [Rect::new(0, 0, 0, 0); 4];
        let mut x = toolbar.x + ctx.px(8) as i32;
        let bh = ctx.px(32);
        for (index, (_, label)) in BUTTONS.iter().enumerate() {
            let w = ctx.face(Role::Body).width(label) + ctx.px(44);
            buttons[index] = Rect::new(x, toolbar.y + (toolbar.h - bh) as i32 / 2, w, bh);
            x += (w + ctx.px(4)) as i32;
        }
        Self { toolbar, buttons, text, status }
    }
}

impl App for Editor {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);

        // ── Панель ──────────────────────────────────────────────────────────
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        for ((icon, label), rect) in BUTTONS.iter().zip(plan.buttons) {
            paint::button(bar, s, rect, Weight::Ghost, "", false);
            let g = ctx.px(16);
            let x = rect.x + ctx.px(12) as i32;
            iconset::draw(s, *icon, x, rect.y + (rect.h - g) as i32 / 2, g, p.ink2, 255);
            paint::text(bar, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
        }
        let title = self.title();
        paint::text_right(bar, s, Role::Strong, plan.toolbar.right() - ctx.px(14) as i32, paint::baseline(ctx, Role::Strong, plan.toolbar), &title, if self.dirty { p.acc_ink } else { p.ink2 });

        // ── Текст ───────────────────────────────────────────────────────────
        let face = ctx.face(Role::Mono);
        let line_h = i32::from(face.line) + ctx.px(2) as i32;
        let char_w = face.width("0").max(1);
        let gutter_w = char_w * (GUTTER_CHARS as u32 + 1) + ctx.px(12);
        let gutter = Rect::new(plan.text.x, plan.text.y, gutter_w, plan.text.h);
        s.fill(gutter, ctx.flat(p.panel));
        draw::vline(s, gutter.right() - 1, gutter.y, gutter.h, p.line.color, p.line.alpha);
        let text_x = gutter.right() + ctx.px(10) as i32;
        let rows = ((plan.text.h as i32 - ctx.px(12) as i32) / line_h).max(1) as usize;
        let cols = ((plan.text.right() - text_x - ctx.px(10) as i32).max(0) as u32 / char_w).max(1) as usize;
        self.page_rows.set(rows);
        self.page_cols.set(cols);
        let top_y = plan.text.y + ctx.px(6) as i32;
        for offset in 0..rows {
            let index = self.top + offset;
            let Some(line) = self.lines.get(index) else {
                break;
            };
            let y = top_y + offset as i32 * line_h;
            if index == self.row {
                draw::blend_rect(s, Rect::new(gutter.right(), y - 1, (plan.text.right() - gutter.right()).max(0) as u32, line_h as u32), p.hover1.color, p.hover1.alpha);
            }
            let number = format!("{}", index + 1);
            paint::text_right(ctx, s, Role::Mono, gutter.right() - ctx.px(8) as i32, y, &number, if index == self.row { p.ink2 } else { p.ink5 });
            let visible: String = line.chars().skip(self.left).take(cols).map(|c| if c == '\t' { ' ' } else { c }).collect();
            paint::text_clipped(ctx, s, Role::Mono, text_x, y, cols as u32 * char_w, &visible, p.ink);
            if index == self.row && self.prompt.is_none() {
                let before: String = line.chars().skip(self.left).take(self.col.saturating_sub(self.left)).map(|c| if c == '\t' { ' ' } else { c }).collect();
                let cx = text_x + face.width(&before) as i32;
                s.fill(Rect::new(cx, y, ctx.px(2), face.line as u32), p.acc);
            }
        }

        // ── Строка состояния ────────────────────────────────────────────────
        let left = match &self.note {
            Some(note) => note.clone(),
            None => String::from("Ctrl+S — сохранить    Ctrl+O — открыть    Ctrl+N — новый"),
        };
        kit::status_bar(ctx, s, plan.status, &left);
        let right = format!("Строка {}, столбец {}  ·  UTF-8  ·  строк {}", self.row + 1, self.col + 1, self.lines.len());
        paint::text_right(ctx, s, Role::Caption, plan.status.right() - ctx.px(14) as i32, paint::baseline(ctx, Role::Caption, plan.status), &right, p.ink4);

        // ── Поле пути ───────────────────────────────────────────────────────
        if let Some(prompt) = &self.prompt {
            draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 90);
            let w = ctx.px(520).min(area.w.saturating_sub(ctx.px(40)));
            let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + area.h as i32 / 4, w, ctx.px(170));
            let inner = kit::card(ctx, s, card);
            let pad = ctx.px(20) as i32;
            let heading = if prompt.ask == Ask::Open { "Открыть файл" } else { "Сохранить как" };
            paint::text(inner, s, Role::Title, card.x + pad, card.y + pad, heading, p.ink);
            let field = Rect::new(card.x + pad, card.y + pad + ctx.px(40) as i32, w - ctx.px(40), ctx.px(34));
            paint::sunk(inner, s, field, ctx.px(6));
            draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
            let tx = field.x + ctx.px(10) as i32;
            paint::text_clipped(inner, s, Role::Mono, tx, paint::baseline(ctx, Role::Mono, field), field.w - ctx.px(20), &prompt.text, p.ink);
            let tw = face.width(&prompt.text).min(field.w - ctx.px(20));
            s.fill(Rect::new(tx + tw as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h - ctx.px(16)), p.acc);
            let hint = prompt.error.clone().unwrap_or_else(|| String::from("Enter — готово    Esc — отмена"));
            let ink = if prompt.error.is_some() { p.bad_ink } else { p.ink4 };
            paint::text_clipped(inner, s, Role::Caption, card.x + pad, field.bottom() + ctx.px(14) as i32, w - ctx.px(40), &hint, ink);
        }
    }

    fn key(&mut self, code: u32) -> bool {
        if self.prompt.is_some() {
            self.prompt_key(code);
            return true;
        }
        self.note = None;
        let page = self.page_rows.get().max(1);
        match code {
            CTRL_S => self.save(),
            CTRL_O => self.ask_open(),
            CTRL_N => self.new_file(),
            ENTER => self.newline(),
            BACKSPACE => self.backspace(),
            WIN_KEY_DELETE => self.delete(),
            WIN_KEY_LEFT => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
            }
            WIN_KEY_RIGHT => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            WIN_KEY_UP => self.move_to(self.row.saturating_sub(1), self.col),
            WIN_KEY_DOWN => self.move_to(self.row + 1, self.col),
            WIN_KEY_PAGE_UP => self.move_to(self.row.saturating_sub(page), self.col),
            WIN_KEY_PAGE_DOWN => self.move_to(self.row + page, self.col),
            WIN_KEY_HOME => {
                // Первое нажатие — к началу текста строки, второе — к нулю.
                let indent = self.lines[self.row].chars().take_while(|c| c.is_whitespace()).count();
                self.col = if self.col == indent { 0 } else { indent };
            }
            WIN_KEY_END => self.col = self.line_len(self.row),
            TAB => {
                for _ in 0..4 {
                    self.insert(' ');
                }
            }
            code if code < 0x0100_0000 => match char::from_u32(code).filter(|ch| !ch.is_control()) {
                Some(ch) => self.insert(ch),
                None => return false,
            },
            _ => return false,
        }
        self.follow();
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.prompt.is_some() {
            return true;
        }
        let plan = Plan::new(ctx, area);
        for (index, rect) in plan.buttons.iter().enumerate() {
            if rect.contains(x, y) {
                match index {
                    0 => self.new_file(),
                    1 => self.ask_open(),
                    2 => self.save(),
                    _ => self.ask_save_as(),
                }
                return true;
            }
        }
        if plan.text.contains(x, y) {
            let face = ctx.face(Role::Mono);
            let line_h = i32::from(face.line) + ctx.px(2) as i32;
            let char_w = face.width("0").max(1) as i32;
            let gutter_w = char_w * (GUTTER_CHARS as i32 + 1) + ctx.px(12) as i32;
            let text_x = plan.text.x + gutter_w + ctx.px(10) as i32;
            let row = self.top + ((y - plan.text.y - ctx.px(6) as i32).max(0) / line_h) as usize;
            let col = self.left + ((x - text_x).max(0) + char_w / 2) as usize / char_w as usize;
            self.move_to(row, col);
            return true;
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
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
