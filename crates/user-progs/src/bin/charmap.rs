//! «Таблица символов» (фаза П1, 20.11).
//!
//! # Зачем
//!
//! Знак, которого нет на клавиатуре, — «№», «—», «→», «±», «°», «€» — в
//! Windows берут из charmap. Здесь то же: сетка знаков шрифта по группам,
//! поиск по имени или коду, крупный образец выбранного и строка «Для
//! копирования», которая уходит в буфер обмена стола одной кнопкой.
//!
//! # Чем рисуется
//!
//! Шрифтами TrueType системы (`/usr/share/fonts/fossai`, как у «Блокнота»):
//! вшитый растр интерфейса знает две сотни знаков, а DejaVu Sans — тысячи.
//! Показываются только знаки, которые в выбранном шрифте **есть**: пустой
//! квадрат в сетке читался бы как «знак есть, но не рисуется». Шрифтов на
//! диске нет — сетка из вшитого растра, и окно так и пишет.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Dirent, KIND_DIRECTORY, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN,
    WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, file_size, open, println, read, readdir,
};

const SPEC: Spec = Spec { name: "charmap", title: "Character Map", period_ms: 10_000 };

const FONTS_DIR: &str = "/usr/share/fonts/fossai";
const MAX_FONT: usize = 8 * 1024 * 1024;

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const TAB: u32 = '\t' as u32;
const CTRL_C: u32 = 0x03;

const CELL: u32 = 44;
const LEFT_W: u32 = 210;
const RIGHT_W: u32 = 270;
const BOTTOM_H: u32 = 60;
const BLOCK_ROW_H: u32 = 32;
const MAX_COPY: usize = 256;

/// Группы знаков: имя и диапазоны кодов.
const BLOCKS: &[(&str, &[(u32, u32)])] = &[
    ("Латиница", &[(0x21, 0x7E), (0xA1, 0xFF), (0x100, 0x17F)]),
    ("Кириллица", &[(0x400, 0x45F), (0x460, 0x4FF)]),
    ("Греческий", &[(0x370, 0x3FF)]),
    ("Знаки препинания", &[(0x2010, 0x205E)]),
    ("Валюты", &[(0x20A0, 0x20C0)]),
    ("Буквоподобные, дроби", &[(0x2100, 0x218B)]),
    ("Стрелки", &[(0x2190, 0x21FF)]),
    ("Математика", &[(0x2200, 0x22FF)]),
    ("Технические", &[(0x2300, 0x23FF)]),
    ("Рамки и блоки", &[(0x2500, 0x259F)]),
    ("Фигуры", &[(0x25A0, 0x25FF)]),
    ("Разное", &[(0x2600, 0x26FF), (0x2700, 0x27BF)]),
];

/// Имена знаков, которые ищут чаще всего.
const NAMES: &[(char, &str)] = &[
    ('№', "Знак номера"),
    ('—', "Длинное тире"),
    ('–', "Короткое тире"),
    ('«', "Открывающая кавычка-ёлочка"),
    ('»', "Закрывающая кавычка-ёлочка"),
    ('„', "Открывающая кавычка-лапка"),
    ('“', "Закрывающая кавычка-лапка"),
    ('…', "Многоточие"),
    ('°', "Знак градуса"),
    ('±', "Плюс-минус"),
    ('×', "Знак умножения"),
    ('÷', "Знак деления"),
    ('≈', "Почти равно"),
    ('≠', "Не равно"),
    ('≤', "Меньше или равно"),
    ('≥', "Больше или равно"),
    ('∞', "Бесконечность"),
    ('√', "Квадратный корень"),
    ('∑', "Знак суммы"),
    ('π', "Греческая строчная буква пи"),
    ('µ', "Знак микро"),
    ('§', "Знак параграфа"),
    ('¶', "Знак абзаца"),
    ('©', "Знак авторского права"),
    ('®', "Зарегистрированный товарный знак"),
    ('™', "Товарный знак"),
    ('€', "Знак евро"),
    ('₽', "Знак рубля"),
    ('$', "Знак доллара"),
    ('£', "Знак фунта"),
    ('¥', "Знак иены"),
    ('←', "Стрелка влево"),
    ('→', "Стрелка вправо"),
    ('↑', "Стрелка вверх"),
    ('↓', "Стрелка вниз"),
    ('↔', "Стрелка влево-вправо"),
    ('⇒', "Двойная стрелка вправо"),
    ('•', "Маркер списка"),
    ('·', "Точка по центру"),
    ('✓', "Галочка"),
    ('✗', "Косой крестик"),
    ('★', "Чёрная звезда"),
    ('☆', "Белая звезда"),
    ('♥', "Червы"),
    ('☺', "Улыбающееся лицо"),
    ('½', "Одна вторая"),
    ('¼', "Одна четвёртая"),
    ('¾', "Три четвёртых"),
    ('²', "Верхний индекс два"),
    ('³', "Верхний индекс три"),
    ('‰', "Промилле"),
    ('′', "Штрих"),
    ('″', "Двойной штрих"),
    ('\u{A0}', "Неразрывный пробел"),
    ('ё', "Кириллическая строчная буква ё"),
    ('Ё', "Кириллическая заглавная буква Ё"),
    ('℃', "Градус Цельсия"),
];

/// Шрифт TrueType и растры глифов по размерам.
struct TtfFace {
    font: ttf::Font,
    cache: RefCell<BTreeMap<(u32, u32), ttf::Bitmap>>,
}

impl TtfFace {
    fn has(&self, ch: char) -> bool {
        self.font.glyph_index(ch) != 0
    }

    /// Нарисовать знак размером `px`, по центру `cell` по ширине, на общей
    /// базовой линии по высоте.
    fn draw(&self, s: &mut Surface, cell: Rect, ch: char, px: u32, ink: Color) {
        let metrics = self.font.line_metrics(px as f32);
        let mut cache = self.cache.borrow_mut();
        // Кэш не растёт без конца: сетка меняется с группой.
        if cache.len() > 1500 {
            cache.clear();
        }
        let bitmap = cache.entry((u32::from(ch), px)).or_insert_with(|| self.font.rasterize(ch, px as f32));
        let base = cell.y + (cell.h as i32 + metrics.ascent as i32 - metrics.descent as i32) / 2;
        let x = cell.x + (cell.w as i32 - bitmap.width as i32) / 2;
        blit(s, bitmap, x, base + bitmap.top, ink, cell);
    }

    /// Знак на базовой линии `baseline` от пера `x`; ответ — шаг пера.
    fn draw_at(&self, s: &mut Surface, x: i32, baseline: i32, ch: char, px: u32, ink: Color, clip: Rect) -> i32 {
        let mut cache = self.cache.borrow_mut();
        if cache.len() > 1500 {
            cache.clear();
        }
        let bitmap = cache.entry((u32::from(ch), px)).or_insert_with(|| self.font.rasterize(ch, px as f32));
        blit(s, bitmap, x + bitmap.left, baseline + bitmap.top, ink, clip);
        (bitmap.advance + 0.5) as i32
    }
}

/// Есть ли знак в начертании интерфейса `face`.
fn ui_has(face: &mini_ui::typeface::Face, ch: char) -> bool {
    u16::try_from(u32::from(ch)).is_ok_and(|code| face.glyphs.iter().any(|glyph| glyph.code == code))
}

/// Положить растр по его покрытию, не выходя за `clip`.
fn blit(s: &mut Surface, glyph: &ttf::Bitmap, x: i32, y: i32, color: Color, clip: Rect) {
    let w = glyph.width as i32;
    if w == 0 {
        return;
    }
    for (row, line) in glyph.coverage.chunks(w as usize).enumerate() {
        let py = y + row as i32;
        if py < clip.y || py >= clip.bottom() || py < 0 || py >= s.height() as i32 {
            continue;
        }
        for (column, coverage) in line.iter().enumerate() {
            let px = x + column as i32;
            if *coverage == 0 || px < clip.x || px >= clip.right() || px < 0 || px >= s.width() as i32 {
                continue;
            }
            let under = Color::from_pixel(s.get(px as u32, py as u32));
            s.put(px as u32, py as u32, under.mix(color, *coverage).pixel());
        }
    }
}

struct Plan {
    toolbar: Rect,
    font: Rect,
    search: Rect,
    left: Rect,
    grid: Rect,
    right: Rect,
    bottom: Rect,
    copy_field: Rect,
    add: Rect,
    copy: Rect,
    clear: Rect,
    status: Rect,
    cell: u32,
    columns: usize,
    rows: usize,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let toolbar = frame.toolbar;
        let status = frame.status;
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = toolbar.y + (toolbar.h as i32 - bh as i32) / 2;
        let font = Rect::new(toolbar.x + pad, by, ctx.px(230), bh);
        let search_w = ctx.px(260).min(toolbar.w / 3);
        let search = Rect::new(toolbar.right() - pad - search_w as i32, by, search_w, bh);
        let bottom = Rect::new(area.x, status.y - ctx.px(BOTTOM_H) as i32, area.w, ctx.px(BOTTOM_H));
        let body_top = toolbar.bottom();
        let body_h = (bottom.y - body_top).max(0) as u32;
        let left_w = ctx.px(LEFT_W).min(area.w / 4);
        let right_w = if area.w >= ctx.px(900) { ctx.px(RIGHT_W) } else { 0 };
        let left = Rect::new(area.x, body_top, left_w, body_h);
        let right = Rect::new(area.right() - right_w as i32, body_top, right_w, body_h);
        let grid_outer = Rect::new(left.right(), body_top, area.w.saturating_sub(left_w + right_w), body_h);
        let cell = ctx.px(CELL);
        let inner_w = grid_outer.w.saturating_sub(ctx.px(32));
        let inner_h = grid_outer.h.saturating_sub(ctx.px(24));
        let columns = (inner_w / cell).max(1) as usize;
        let rows = (inner_h / cell).max(1) as usize;
        let grid_w = cell * columns as u32;
        let grid = Rect::new(grid_outer.x + (grid_outer.w as i32 - grid_w as i32) / 2, body_top + ctx.px(12) as i32, grid_w, cell * rows as u32);
        let bw = ctx.px(120);
        let gap = ctx.px(8) as i32;
        let byb = bottom.y + (bottom.h as i32 - bh as i32) / 2;
        let copy = Rect::new(bottom.right() - pad - bw as i32, byb, bw, bh);
        let add = Rect::new(copy.x - gap - bw as i32, byb, bw, bh);
        let clear = Rect::new(add.x - gap - bw as i32, byb, bw, bh);
        let label_w = ctx.face(Role::Body).width("Для копирования:") + ctx.px(14);
        let fx = bottom.x + pad + label_w as i32;
        let copy_field = Rect::new(fx, byb, (clear.x - gap - fx).max(0) as u32, bh);
        Self { toolbar, font, search, left, grid, right, bottom, copy_field, add, copy, clear, status, cell, columns, rows }
    }
}

struct CharMap {
    fonts: Vec<(String, String)>,
    font_index: usize,
    face: Option<TtfFace>,
    /// Группа: `None` — все.
    block: Option<usize>,
    /// Знаки в сетке.
    chars: Vec<char>,
    selected: usize,
    /// Первая показанная строка сетки.
    top: usize,
    query: String,
    searching: bool,
    copy: String,
    note: Option<String>,
    columns: core::cell::Cell<usize>,
    rows: core::cell::Cell<usize>,
}

impl CharMap {
    fn new() -> Self {
        let fonts = list_fonts();
        let font_index = fonts.iter().position(|(name, _)| name.to_lowercase().starts_with("dejavusans.")).unwrap_or(0);
        let mut map = Self {
            fonts,
            font_index,
            face: None,
            block: None,
            chars: Vec::new(),
            selected: 0,
            top: 0,
            query: String::new(),
            searching: false,
            copy: String::new(),
            note: None,
            columns: core::cell::Cell::new(16),
            rows: core::cell::Cell::new(8),
        };
        map.load_font();
        // Сначала — кириллица: ради неё сюда приходят чаще, чем ради латиницы.
        map.block = Some(1);
        map.rebuild();
        println(&format!("charmap: ready, font '{}', {} glyph(s) shown", map.font_name(), map.chars.len()));
        map
    }

    fn font_name(&self) -> String {
        match self.fonts.get(self.font_index) {
            Some((name, _)) if self.face.is_some() => pretty_font(name),
            _ => String::from("Шрифт интерфейса"),
        }
    }

    fn load_font(&mut self) {
        self.face = self.fonts.get(self.font_index).and_then(|(_, path)| {
            let data = read_bytes(path, MAX_FONT)?;
            Some(TtfFace { font: ttf::Font::parse(data)?, cache: RefCell::new(BTreeMap::new()) })
        });
    }

    fn has(&self, ch: char) -> bool {
        match &self.face {
            Some(face) => face.has(ch),
            None => self_face_has(ch),
        }
    }

    /// Пересобрать сетку по группе и поиску.
    fn rebuild(&mut self) {
        let previous = self.chars.get(self.selected).copied();
        let mut out = Vec::new();
        let query = self.query.trim().to_lowercase();
        if !query.is_empty() {
            let code = parse_code(&query);
            for (_, ranges) in BLOCKS {
                for &(start, end) in *ranges {
                    for code_point in start..=end {
                        let Some(ch) = char::from_u32(code_point) else { continue };
                        let hit = code == Some(code_point)
                            || (code.is_none() && (name_of(ch).to_lowercase().contains(&query) || query.chars().count() == 1 && query.starts_with(ch.to_lowercase().next().unwrap_or(ch))));
                        if hit && self.has(ch) && !out.contains(&ch) {
                            out.push(ch);
                        }
                    }
                }
            }
        } else {
            let blocks: Vec<usize> = match self.block {
                Some(index) => alloc::vec![index],
                None => (0..BLOCKS.len()).collect(),
            };
            for index in blocks {
                for &(start, end) in BLOCKS[index].1 {
                    for code_point in start..=end {
                        if let Some(ch) = char::from_u32(code_point) {
                            if self.has(ch) {
                                out.push(ch);
                            }
                        }
                    }
                }
            }
        }
        self.chars = out;
        self.selected = previous.and_then(|ch| self.chars.iter().position(|other| *other == ch)).unwrap_or(0);
        self.top = 0;
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        let columns = self.columns.get().max(1);
        let rows = self.rows.get().max(1);
        let row = self.selected / columns;
        if row < self.top {
            self.top = row;
        } else if row >= self.top + rows {
            self.top = row + 1 - rows;
        }
    }

    fn step(&mut self, delta: isize) {
        if self.chars.is_empty() {
            return;
        }
        let last = self.chars.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.keep_visible();
    }

    fn add_selected(&mut self) {
        if let Some(ch) = self.chars.get(self.selected).copied() {
            if self.copy.chars().count() < MAX_COPY {
                self.copy.push(ch);
                println(&format!("charmap: added '{ch}' (U+{:04X})", u32::from(ch)));
            }
        }
    }

    fn copy_out(&mut self) {
        let text = if self.copy.is_empty() {
            self.chars.get(self.selected).map(|ch| ch.to_string()).unwrap_or_default()
        } else {
            self.copy.clone()
        };
        if text.is_empty() {
            return;
        }
        let ok = user_progs::clipboard_set(&text);
        let count = text.chars().count();
        println(&format!("charmap: copied '{text}' ({count} char(s)) to the clipboard{}", if ok { "" } else { " - FAILED" }));
        self.note = Some(if ok { format!("Скопировано в буфер обмена: {text}") } else { String::from("Буфер обмена не ответил") });
    }

    fn next_font(&mut self) {
        if self.fonts.len() < 2 {
            return;
        }
        self.font_index = (self.font_index + 1) % self.fonts.len();
        self.load_font();
        self.rebuild();
        println(&format!("charmap: font '{}', {} glyph(s) shown", self.font_name(), self.chars.len()));
    }

    fn set_block(&mut self, block: Option<usize>) {
        self.block = block;
        self.query.clear();
        self.searching = false;
        self.selected = 0;
        self.rebuild();
        self.selected = 0;
        self.top = 0;
        let title = block.map_or("Все", |index| BLOCKS[index].0);
        println(&format!("charmap: block '{title}', {} glyph(s)", self.chars.len()));
    }

    fn draw_char(&self, s: &mut Surface, ctx: Ctx, cell: Rect, ch: char, px: u32, ink: Color) {
        match &self.face {
            Some(face) => face.draw(s, cell, ch, px, ink),
            None => {
                let mut buf = [0u8; 4];
                let role = if px >= ctx.px(40) { Role::Heading } else { Role::Title };
                paint::text_centered(ctx, s, role, cell, ch.encode_utf8(&mut buf), ink);
            }
        }
    }

    /// Строка шрифтом интерфейса, а знаки, которых во вшитом растре нет
    /// («ā», «→», «€»), — выбранным шрифтом TrueType (20.11: строка
    /// состояния и имя знака показывали «?» вместо самого знака). `y` — верх
    /// строки, как у [`paint::text`]. Ответ — ширина.
    #[allow(clippy::too_many_arguments)]
    fn text_mixed(&self, ctx: Ctx, s: &mut Surface, role: Role, x: i32, y: i32, room: u32, text: &str, ink: Color) -> u32 {
        let Some(ttf) = &self.face else {
            return paint::text_clipped(ctx, s, role, x, y, room, text, ink);
        };
        let face = ctx.face(role);
        let baseline = y + i32::from(face.ascent);
        // Кегль TTF — по высоте строчных букв интерфейса: ascent ≈ 0.8 em.
        let px = (u32::from(face.ascent) * 5 / 4).max(8);
        let clip = Rect::new(x, y, room, u32::from(face.line));
        let end = x + room as i32;
        let mut pen = x;
        let mut run = String::new();
        let flush = |run: &mut String, pen: &mut i32, s: &mut Surface| {
            if !run.is_empty() && *pen < end {
                *pen += paint::text_clipped(ctx, s, role, *pen, y, (end - *pen) as u32, run, ink) as i32;
            }
            run.clear();
        };
        for ch in text.chars() {
            if ui_has(face, ch) || !ttf.has(ch) {
                run.push(ch);
            } else {
                flush(&mut run, &mut pen, s);
                if pen >= end {
                    break;
                }
                pen += ttf.draw_at(s, pen, baseline, ch, px, ink, clip);
            }
        }
        flush(&mut run, &mut pen, s);
        (pen - x).max(0) as u32
    }

    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        paint::button(bar, s, plan.font, Weight::Normal, "", false);
        let g = ctx.px(16);
        iconset::draw(s, Fi::FileText, plan.font.x + ctx.px(10) as i32, plan.font.y + (plan.font.h - g) as i32 / 2, g, p.ink3, 255);
        let tx = plan.font.x + ctx.px(34) as i32;
        paint::text_clipped(bar, s, Role::Body, tx, paint::baseline(bar, Role::Body, plan.font), plan.font.w.saturating_sub(ctx.px(60)), &self.font_name(), p.ink);
        let chev = ctx.px(12);
        iconset::draw(s, Fi::ChevronRight, plan.font.right() - ctx.px(22) as i32, plan.font.y + (plan.font.h - chev) as i32 / 2, chev, p.ink4, 255);

        let field = plan.search;
        paint::sunk(bar, s, field, ctx.px(6));
        if self.searching {
            draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
        }
        iconset::draw(s, Fi::Search, field.x + ctx.px(10) as i32, field.y + (field.h - g) as i32 / 2, g, p.ink4, 255);
        let sx = field.x + ctx.px(34) as i32;
        let room = (field.right() - sx - ctx.px(8) as i32).max(0) as u32;
        let y = paint::baseline(bar, Role::Body, field);
        if self.query.is_empty() && !self.searching {
            paint::text_clipped(bar, s, Role::Body, sx, y, room, "Имя знака или код (2192)", p.ink5);
        } else {
            let w = paint::text_clipped(bar, s, Role::Body, sx, y, room, &self.query, p.ink);
            if self.searching {
                s.fill(Rect::new(sx + w.min(room) as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h.saturating_sub(ctx.px(16))), p.acc);
            }
        }
    }

    fn block_rows(ctx: Ctx, plan: &Plan) -> Vec<(Option<usize>, Rect)> {
        let row_h = ctx.px(BLOCK_ROW_H);
        let mut out = Vec::new();
        let x = plan.left.x + ctx.px(8) as i32;
        let w = plan.left.w.saturating_sub(ctx.px(16));
        let mut y = plan.left.y + ctx.px(10) as i32;
        for block in core::iter::once(None).chain((0..BLOCKS.len()).map(Some)) {
            if y + row_h as i32 > plan.left.bottom() {
                break;
            }
            out.push((block, Rect::new(x, y, w, row_h)));
            y += row_h as i32;
        }
        out
    }

    fn draw_left(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        draw::vline(s, plan.left.right() - 1, plan.left.y, plan.left.h, p.line.color, p.line.alpha);
        for (block, rect) in Self::block_rows(ctx, plan) {
            let on = self.query.is_empty() && block == self.block;
            paint::row(ctx, s, rect, if on { paint::RowState::Selected } else { paint::RowState::Idle });
            let title = block.map_or("Все группы", |index| BLOCKS[index].0);
            paint::text_clipped(ctx, s, Role::Body, rect.x + ctx.px(12) as i32, paint::baseline(ctx, Role::Body, rect), rect.w.saturating_sub(ctx.px(20)), title, if on { p.ink } else { p.ink2 });
        }
    }

    fn draw_grid(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if self.chars.is_empty() {
            let text = if self.query.is_empty() { "В этом шрифте нет знаков этой группы" } else { "Ничего не найдено" };
            paint::text(ctx, s, Role::Body, plan.grid.x + ctx.px(8) as i32, plan.grid.y + ctx.px(8) as i32, text, p.ink4);
            return;
        }
        let px = plan.cell * 11 / 20;
        for row in 0..plan.rows {
            for column in 0..plan.columns {
                let index = (self.top + row) * plan.columns + column;
                let Some(ch) = self.chars.get(index).copied() else { break };
                let cell = Rect::new(plan.grid.x + (column as u32 * plan.cell) as i32, plan.grid.y + (row as u32 * plan.cell) as i32, plan.cell, plan.cell);
                let inner = Rect::new(cell.x + 2, cell.y + 2, cell.w.saturating_sub(4), cell.h.saturating_sub(4));
                let on = index == self.selected;
                if on {
                    draw::rounded(s, inner, ctx.px(6), ctx.flat(p.acctint), 255);
                    draw::rounded_stroke(s, inner, ctx.px(6), p.accedge, 255);
                } else {
                    draw::rounded_stroke(s, inner, ctx.px(6), p.line.color, p.line.alpha);
                }
                self.draw_char(s, ctx, inner, ch, px, if on { p.ink } else { p.ink2 });
            }
        }
        // Где мы в длинной группе.
        let total_rows = self.chars.len().div_ceil(plan.columns);
        if total_rows > plan.rows {
            let track = Rect::new(plan.grid.right() + ctx.px(6) as i32, plan.grid.y, ctx.px(4), plan.grid.h);
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = (plan.grid.h as usize * plan.rows / total_rows).max(ctx.px(24) as usize) as u32;
            let max_top = total_rows - plan.rows;
            let thumb_y = track.y + ((track.h - thumb_h.min(track.h)) as usize * self.top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h.min(track.h)), ctx.px(2), p.ink4, 255);
        }
    }

    fn draw_right(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.right.w == 0 {
            return;
        }
        draw::vline(s, plan.right.x, plan.right.y, plan.right.h, p.line.color, p.line.alpha);
        let Some(ch) = self.chars.get(self.selected).copied() else { return };
        let pad = ctx.px(18) as i32;
        let card = Rect::new(plan.right.x + pad, plan.right.y + pad, plan.right.w.saturating_sub(pad as u32 * 2), ctx.px(150));
        paint::card(ctx, s, card);
        let inner = ctx.on(ctx.flat(p.card));
        self.draw_char(s, inner, card, ch, ctx.px(84), p.ink);
        let mut y = card.bottom() + ctx.px(14) as i32;
        let w = card.w;
        let line = i32::from(ctx.face(Role::Body).line);
        let name = name_of(ch);
        y += self.draw_wrapped(ctx, s, card.x, y, w, &name, Role::Strong, p.ink);
        y += ctx.px(8) as i32;
        let code = u32::from(ch);
        let mut utf8 = [0u8; 4];
        let bytes: Vec<String> = ch.encode_utf8(&mut utf8).bytes().map(|b| format!("{b:02X}")).collect();
        let facts = [
            ("Код", format!("U+{code:04X}")),
            ("Десятичный", format!("{code}")),
            ("UTF-8", bytes.join(" ")),
            ("Группа", block_of(ch).to_string()),
        ];
        let label_w = ctx.px(100);
        for (label, value) in facts {
            paint::text_clipped(ctx, s, Role::Body, card.x, y, label_w, label, p.ink3);
            paint::text_clipped(ctx, s, Role::Mono, card.x + label_w as i32, y, w.saturating_sub(label_w), &value, p.ink);
            y += line + ctx.px(8) as i32;
        }
        y += ctx.px(6) as i32;
        paint::text_clipped(ctx, s, Role::Caption, card.x, y, w, "Enter — добавить, Ctrl+C — копировать", p.ink4);
    }

    fn draw_bottom(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar_ctx = ctx.on(ctx.flat(p.panel));
        s.fill(plan.bottom, ctx.flat(p.panel));
        draw::hline(s, plan.bottom.x, plan.bottom.y, plan.bottom.w, p.line.color, p.line.alpha);
        paint::text(bar_ctx, s, Role::Body, plan.bottom.x + ctx.px(12) as i32, paint::baseline(bar_ctx, Role::Body, plan.copy_field), "Для копирования:", p.ink2);
        paint::sunk(bar_ctx, s, plan.copy_field, ctx.px(6));
        let fx = plan.copy_field.x + ctx.px(10) as i32;
        let room = plan.copy_field.w.saturating_sub(ctx.px(20));
        let fctx = bar_ctx.on(ctx.flat(p.sunk));
        if self.copy.is_empty() {
            paint::text_clipped(fctx, s, Role::Body, fx, paint::baseline(fctx, Role::Body, plan.copy_field), room, "Пусто — «Выбрать» добавит знак", p.ink5);
        } else if let Some(face) = &self.face {
            // Строка — тем же шрифтом, что и сетка: знаков из неё в растре
            // интерфейса может не быть.
            let px = ctx.px(18);
            let mut x = fx;
            let chars: Vec<char> = self.copy.chars().collect();
            let advance = |ch: char| (face.font.advance(ch, px as f32) + 0.99) as i32;
            let total: i32 = chars.iter().map(|ch| advance(*ch)).sum();
            let mut skip = 0usize;
            let mut width = total;
            while width > room as i32 && skip < chars.len() {
                width -= advance(chars[skip]);
                skip += 1;
            }
            for ch in &chars[skip..] {
                let w = advance(*ch);
                let cell = Rect::new(x, plan.copy_field.y, w.max(1) as u32, plan.copy_field.h);
                face.draw(s, cell, *ch, px, p.ink);
                x += w;
            }
        } else {
            paint::text_clipped(fctx, s, Role::Body, fx, paint::baseline(fctx, Role::Body, plan.copy_field), room, &self.copy, p.ink);
        }
        paint::button(bar_ctx, s, plan.clear, Weight::Ghost, "Очистить", false);
        paint::button(bar_ctx, s, plan.add, Weight::Normal, "Выбрать", false);
        paint::button(bar_ctx, s, plan.copy, Weight::Primary, "Копировать", false);
    }
}

impl App for CharMap {
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & user_abi::WIN_MOD_CTRL != 0;
        if code == CTRL_C || ctrl && (latin == 'c' as u32 || latin == 'C' as u32) {
            self.copy_out();
            return true;
        }
        if self.searching {
            match code {
                ESCAPE => {
                    self.searching = false;
                    self.query.clear();
                }
                ENTER => {
                    self.searching = false;
                    self.add_selected();
                    return true;
                }
                BACKSPACE => {
                    self.query.pop();
                }
                WIN_KEY_DOWN | WIN_KEY_UP | WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                    self.searching = false;
                    return self.key_with(code, mods, latin);
                }
                code if code < 0x0100_0000 => match char::from_u32(code) {
                    Some(ch) if !ch.is_control() && self.query.chars().count() < 40 => self.query.push(ch),
                    _ => return false,
                },
                _ => return false,
            }
            self.rebuild();
            println(&format!("charmap: search '{}' -> {} match(es)", self.query, self.chars.len()));
            return true;
        }
        let columns = self.columns.get().max(1) as isize;
        let rows = self.rows.get().max(1) as isize;
        match code {
            WIN_KEY_LEFT => self.step(-1),
            WIN_KEY_RIGHT => self.step(1),
            WIN_KEY_UP => self.step(-columns),
            WIN_KEY_DOWN => self.step(columns),
            WIN_KEY_PAGE_UP => self.step(-columns * rows),
            WIN_KEY_PAGE_DOWN => self.step(columns * rows),
            WIN_KEY_HOME => self.step(isize::MIN / 2),
            WIN_KEY_END => self.step(isize::MAX / 2),
            ENTER | 0x20 => self.add_selected(),
            BACKSPACE => {
                self.copy.pop();
            }
            ESCAPE => {
                self.copy.clear();
                self.note = None;
            }
            TAB => {
                let next = match self.block {
                    None => Some(0),
                    Some(index) if index + 1 < BLOCKS.len() => Some(index + 1),
                    Some(_) => None,
                };
                self.set_block(next);
            }
            code if code == '/' as u32 || code == 'f' as u32 && ctrl => self.searching = true,
            code if code == 'n' as u32 && ctrl => self.next_font(),
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        self.searching = false;
        if plan.search.contains(x, y) {
            self.searching = true;
            return true;
        }
        if plan.font.contains(x, y) {
            self.next_font();
            return true;
        }
        if plan.add.contains(x, y) {
            self.add_selected();
            return true;
        }
        if plan.copy.contains(x, y) {
            self.copy_out();
            return true;
        }
        if plan.clear.contains(x, y) {
            self.copy.clear();
            self.note = None;
            return true;
        }
        for (block, rect) in Self::block_rows(ctx, &plan) {
            if rect.contains(x, y) {
                self.set_block(block);
                return true;
            }
        }
        if plan.grid.contains(x, y) {
            let column = ((x - plan.grid.x) as u32 / plan.cell) as usize;
            let row = ((y - plan.grid.y) as u32 / plan.cell) as usize;
            let index = (self.top + row) * plan.columns + column;
            if index < self.chars.len() {
                // Повторный щелчок по выбранному — добавить, как двойной в Windows.
                if index == self.selected {
                    self.add_selected();
                } else {
                    self.selected = index;
                }
            }
            return true;
        }
        true
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        let columns = self.columns.get().max(1);
        let rows = self.rows.get().max(1);
        let total_rows = self.chars.len().div_ceil(columns);
        let max_top = total_rows.saturating_sub(rows);
        self.top = if clicks > 0 { self.top.saturating_sub(3) } else { (self.top + 3).min(max_top) };
        // Выбор остаётся в видимой части.
        let first = self.top * columns;
        let last = ((self.top + rows) * columns).min(self.chars.len()).saturating_sub(1);
        self.selected = self.selected.clamp(first, last.max(first));
        true
    }

    fn quits_on_q(&self) -> bool {
        !self.searching
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        self.columns.set(plan.columns);
        self.rows.set(plan.rows);
        self.draw_toolbar(s, ctx, &plan);
        self.draw_left(s, ctx, &plan);
        self.draw_grid(s, ctx, &plan);
        self.draw_right(s, ctx, &plan);
        self.draw_bottom(s, ctx, &plan);
        let status = match (&self.note, self.chars.get(self.selected)) {
            (Some(note), _) => note.clone(),
            (None, Some(ch)) => format!("{} · знаков: {} · U+{:04X} {}", self.font_name(), self.chars.len(), u32::from(*ch), name_of(*ch)),
            (None, None) => format!("{} · знаков: 0", self.font_name()),
        };
        // Полоса — общая, а фраза — своим путём: в ней сам знак, которого во
        // вшитом растре может не быть.
        kit::status_bar(ctx, s, plan.status, "");
        if !plan.status.is_empty() {
            let bar = ctx.on(theme::panel_bg());
            let pad = ctx.px(14);
            let y = paint::baseline(bar, Role::MonoSmall, plan.status);
            self.text_mixed(bar, s, Role::MonoSmall, plan.status.x + pad as i32, y, plan.status.w.saturating_sub(pad * 2), &status, ctx.palette.ink4);
        }
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Character Map\x1fТаблица символов"))
    }
}

// ---------------------------------------------------------------------------
// Имена и группы
// ---------------------------------------------------------------------------

fn name_of(ch: char) -> String {
    if let Some((_, name)) = NAMES.iter().find(|(other, _)| *other == ch) {
        return (*name).to_string();
    }
    let code = u32::from(ch);
    let case = |upper: bool| if upper { "заглавная" } else { "строчная" };
    match code {
        0x30..=0x39 => format!("Цифра {ch}"),
        0x41..=0x5A | 0x61..=0x7A => format!("Латинская {} буква {ch}", case(ch.is_uppercase())),
        0xC0..=0x17F if ch.is_alphabetic() => format!("Латинская {} буква {ch}", case(ch.is_uppercase())),
        0x400..=0x4FF if ch.is_alphabetic() => format!("Кириллическая {} буква {ch}", case(ch.is_uppercase())),
        0x370..=0x3FF if ch.is_alphabetic() => format!("Греческая {} буква {ch}", case(ch.is_uppercase())),
        0x2190..=0x21FF => String::from("Стрелка"),
        0x2200..=0x22FF => String::from("Математический знак"),
        0x2500..=0x257F => String::from("Элемент рамки"),
        0x2580..=0x259F => String::from("Элемент блока"),
        0x25A0..=0x25FF => String::from("Геометрическая фигура"),
        0x20A0..=0x20C0 => String::from("Знак валюты"),
        _ if ch.is_ascii_punctuation() => String::from("Знак препинания ASCII"),
        _ => format!("Знак U+{code:04X}"),
    }
}

fn block_of(ch: char) -> &'static str {
    let code = u32::from(ch);
    BLOCKS
        .iter()
        .find(|(_, ranges)| ranges.iter().any(|(start, end)| (*start..=*end).contains(&code)))
        .map_or("Прочее", |(name, _)| name)
}

/// `2192`, `u+2192`, `0x2192` — код; иначе `None`.
fn parse_code(query: &str) -> Option<u32> {
    let text = query.trim_start_matches("u+").trim_start_matches("0x");
    if text.len() < 2 || text.len() > 6 || !text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
}

/// Есть ли знак во вшитом растре интерфейса.
fn self_face_has(ch: char) -> bool {
    let face = mini_ui::typeface::face(Role::Body, mini_ui::typeface::Tier::Normal);
    u16::try_from(u32::from(ch)).is_ok_and(|code| face.glyphs.iter().any(|glyph| glyph.code == code))
}

fn pretty_font(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let mut out = String::new();
    let mut previous_lower = false;
    for ch in stem.chars() {
        if ch == '-' || ch == '_' {
            out.push(' ');
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower {
            out.push(' ');
        }
        previous_lower = ch.is_lowercase();
        out.push(ch);
    }
    out.replace("Deja Vu", "DejaVu").replace("I B M", "IBM")
}

impl CharMap {
    /// Перенос по словам: сколько точек по высоте заняло. Знаки не из растра
    /// интерфейса — шрифтом TrueType ([`CharMap::text_mixed`]).
    #[allow(clippy::too_many_arguments)]
    fn draw_wrapped(&self, ctx: Ctx, s: &mut Surface, x: i32, y: i32, w: u32, text: &str, role: Role, ink: Color) -> i32 {
        let face = ctx.face(role);
        let line_h = i32::from(face.line);
        let mut line = String::new();
        let mut lines = 0;
        for word in text.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if face.width(&candidate) > w && !line.is_empty() {
                self.text_mixed(ctx, s, role, x, y + lines * line_h, w, &line, ink);
                lines += 1;
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            self.text_mixed(ctx, s, role, x, y + lines * line_h, w, &line, ink);
            lines += 1;
        }
        lines * line_h
    }
}

// ---------------------------------------------------------------------------
// Файлы
// ---------------------------------------------------------------------------

fn list_fonts() -> Vec<(String, String)> {
    let fd = open(FONTS_DIR);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if entry.kind != KIND_DIRECTORY && name.to_lowercase().ends_with(".ttf") {
            let path = format!("{FONTS_DIR}/{name}");
            out.push((name, path));
        }
    }
    close(fd);
    out.sort();
    out
}

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

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    app::run(&SPEC, &info, size, |_, _| CharMap::new())
}
