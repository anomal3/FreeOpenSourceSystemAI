//! Всплывающие окна трея (фаза 72h-1, макет 05): раскладка, сеть, календарь.
//!
//! Только то, что в системе есть. Звука без микшера и быстрых настроек
//! (Wi-Fi, Bluetooth, «Не беспокоить») в макете больше, чем у машины, — их
//! окон нет, пока за ними ничего не стоит.
//!
//! Размеры из таблицы макета: скругление 8, поле 16, до панели 12; ячейка
//! календаря 34, сегодня — круг акцента.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw, theme};

use crate::input::keymap::Layout;

/// Какое окно.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Layout,
    Network,
    Calendar,
}

/// Что делает строка окна.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Layout(Layout),
    LanguageSettings,
    NetworkSettings,
    ClockSettings,
}

/// Открытое окно трея.
pub struct TrayPop {
    pub kind: Kind,
    pub rect: Rect,
    surface: Surface,
    /// Строки и кнопки в координатах экрана.
    rows: Vec<(Rect, Action)>,
}

const PAD: u32 = 16;
/// Ячейка календаря.
const CELL: u32 = 40;
const CELL_H: u32 = 34;

const MONTHS: [&str; 12] = [
    "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря",
];
const WEEKDAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
const SHORT: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];

/// День недели, 0 — понедельник (1970-01-01 — четверг).
fn weekday(year: i32, month: u8, day: u8) -> usize {
    let days = calendar::days_from_civil(i64::from(year), month, day);
    (days + 3).rem_euclid(7) as usize
}

/// Дней в месяце.
fn month_days(year: i32, month: u8) -> u8 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    (calendar::days_from_civil(i64::from(next_year), next_month, 1) - calendar::days_from_civil(i64::from(year), month, 1))
        as u8
}

impl TrayPop {
    /// Открыть окно `kind` над значком трея `anchor` (экран); `panel_top` —
    /// верх панели задач.
    #[must_use]
    pub fn new(kind: Kind, anchor: Rect, screen_w: u32, panel_top: i32, scale: u32) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        let px = |value: u32| value * scale;
        let (w, h) = match kind {
            Kind::Layout => (px(220), px(PAD + 24 + 32 * 2 + 9 + 32 + 12)),
            Kind::Network => (px(320), px(PAD + 28 + 8 + 52 + 16 + 32 + PAD)),
            Kind::Calendar => (px(CELL * 7 + PAD * 2), px(PAD + 28 + 18 + 12 + 24 + CELL_H * 6 + 8 + 32 + 12)),
        };
        let x = (anchor.x + anchor.w as i32 / 2 - w as i32 / 2).clamp(px(12) as i32, (screen_w as i32 - w as i32 - px(12) as i32).max(0));
        let y = panel_top - px(12) as i32 - h as i32;
        let p = ctx.palette;
        let fill = p.tb1.over(p.tb2.color);
        let surface = Surface::new(w, h, fill)?;
        let mut pop = Self { kind, rect: Rect::new(x, y, w, h), surface, rows: Vec::new() };
        pop.redraw(scale);
        Some(pop)
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Строка под точкой экрана.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Action> {
        self.rows.iter().find(|(rect, _)| rect.contains(x, y)).map(|(_, action)| *action)
    }

    fn redraw(&mut self, scale: u32) {
        let ctx = Ctx::scaled(scale);
        let p = ctx.palette;
        let px = |value: u32| value * scale;
        let fill = p.tb1.over(p.tb2.color);
        let ctx = ctx.on(fill);
        let bounds = self.surface.bounds();
        self.surface.fill(bounds, fill);
        draw::rounded_stroke(&mut self.surface, bounds, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let (ox, oy) = (self.rect.x, self.rect.y);
        let mut rows = Vec::new();
        let s = &mut self.surface;
        let left = px(PAD) as i32;
        let inner_w = bounds.w - px(PAD * 2);
        match self.kind {
            Kind::Layout => {
                let mut y = px(PAD) as i32 - px(4) as i32;
                paint::text(ctx, s, Role::Caption, left, y + px(4) as i32, "Раскладка", p.ink4);
                y += px(24) as i32;
                let current = crate::input::keymap::layout();
                for (layout, name, tag) in [(Layout::Ru, "Русский", "РУС"), (Layout::Us, "English", "ENG")] {
                    let row = Rect::new(px(8) as i32, y, bounds.w - px(16), px(32));
                    if layout == current {
                        draw::rounded(s, row, px(theme::R_ROW), p.btnhov.over(fill), 255);
                    }
                    paint::text(ctx, s, Role::Body, left + px(8) as i32, paint::baseline(ctx, Role::Body, row), name, p.ink);
                    paint::text_right(ctx, s, Role::Caption, row.right() - px(10) as i32, paint::baseline(ctx, Role::Caption, row), tag, p.ink4);
                    rows.push((row.translate(ox, oy), Action::Layout(layout)));
                    y += px(32) as i32;
                }
                draw::hline(s, left, y + px(4) as i32, inner_w, p.line2.color, p.line2.alpha);
                y += px(9) as i32;
                let row = Rect::new(px(8) as i32, y, bounds.w - px(16), px(32));
                paint::text(ctx, s, Role::Body, left, paint::baseline(ctx, Role::Body, row), "Параметры языка", p.ink3);
                rows.push((row.translate(ox, oy), Action::LanguageSettings));
            }
            Kind::Network => {
                let mut y = px(PAD) as i32;
                let head = Rect::new(left, y, inner_w, px(28));
                paint::text(ctx, s, Role::Title, left, paint::baseline(ctx, Role::Title, head), "Сеть", p.ink);
                y += px(28 + 8) as i32;
                let status = crate::net::status();
                let (title, note, up) = match &status {
                    None => ("Сетевой карты нет", String::from("Подключиться нечем"), false),
                    Some(status) if status.address.is_unspecified() => {
                        ("Проводное подключение", String::from("Адрес не получен — кабель или DHCP"), false)
                    }
                    Some(status) => ("Проводное подключение", alloc::format!("Подключено, адрес {}", status.address), true),
                };
                let card = Rect::new(px(8) as i32, y, bounds.w - px(16), px(52));
                if up {
                    draw::rounded(s, card, px(theme::R_ROW), p.sel1.over(fill), 255);
                }
                let glyph = px(18);
                glyphicon::draw(s, Icon::Display, left, card.y + (card.h as i32 - glyph as i32) / 2, glyph, if up { p.ink } else { p.ink4 }, 255);
                let tx = left + px(30) as i32;
                let room = (card.right() - tx - px(8) as i32).max(0) as u32;
                paint::text_clipped(ctx, s, Role::Body, tx, card.y + px(8) as i32, room, title, p.ink);
                paint::text_clipped(ctx, s, Role::Caption, tx, card.y + px(28) as i32, room, &note, p.ink3);
                y += px(52 + 16) as i32;
                let label = "Параметры сети";
                let bw = ctx.face(Role::Body).width(label) + px(28);
                let button = Rect::new(bounds.right() - px(PAD) as i32 - bw as i32, y, bw, px(32));
                draw::rounded(s, button, px(theme::R_ROW), p.btn.over(fill), 255);
                draw::rounded_stroke(s, button, px(theme::R_ROW), p.btnline.color, p.btnline.alpha);
                paint::text_centered(ctx.on(p.btn.over(fill)), s, Role::Body, button, label, p.ink);
                rows.push((button.translate(ox, oy), Action::NetworkSettings));
            }
            Kind::Calendar => {
                let mut y = px(PAD) as i32;
                let Some(now) = crate::time::now_local() else {
                    paint::text(ctx, s, Role::Body, left, y, "Дата неизвестна: прошивка не дала часов", p.ink3);
                    self.rows = rows;
                    return;
                };
                let head = Rect::new(left, y, inner_w, px(28));
                let title = alloc::format!("{} {} {} г.", now.day, MONTHS[usize::from(now.month.max(1) - 1) % 12], now.year);
                paint::text(ctx, s, Role::Title, left, paint::baseline(ctx, Role::Title, head), &title, p.ink);
                y += px(28) as i32;
                let today = weekday(now.year, now.month, now.day);
                paint::text(ctx, s, Role::Caption, left, y, WEEKDAYS[today], p.ink4);
                y += px(18 + 12) as i32;
                for (index, name) in SHORT.iter().enumerate() {
                    let cell = Rect::new(left + (index as u32 * px(CELL)) as i32, y, px(CELL), px(24));
                    paint::text_centered(ctx, s, Role::Caption, cell, name, p.ink4);
                }
                y += px(24) as i32;
                // Сетка с понедельника недели, где первое число.
                let first = weekday(now.year, now.month, 1);
                let days = month_days(now.year, now.month);
                let (prev_year, prev_month) = if now.month == 1 { (now.year - 1, 12) } else { (now.year, now.month - 1) };
                let prev_days = month_days(prev_year, prev_month);
                for slot in 0..42u32 {
                    let (col, row) = (slot % 7, slot / 7);
                    let cell = Rect::new(left + (col * px(CELL)) as i32, y + (row * px(CELL_H)) as i32, px(CELL), px(CELL_H));
                    let number = slot as i32 - first as i32 + 1;
                    let (label, ink, is_today) = if number < 1 {
                        (i32::from(prev_days) + number, p.ink6, false)
                    } else if number > i32::from(days) {
                        (number - i32::from(days), p.ink6, false)
                    } else {
                        (number, p.ink2, number == i32::from(now.day))
                    };
                    let text = alloc::format!("{label}");
                    if is_today {
                        let side = px(30);
                        let circle = Rect::new(cell.x + (cell.w as i32 - side as i32) / 2, cell.y + (cell.h as i32 - side as i32) / 2, side, side);
                        draw::rounded(s, circle, side / 2, p.acc, 255);
                        paint::text_centered(ctx.on(p.acc), s, Role::Label, cell, &text, Color::rgb(0xFF, 0xFF, 0xFF));
                    } else {
                        paint::text_centered(ctx, s, Role::Body, cell, &text, ink);
                    }
                }
                y += px(CELL_H * 6 + 8) as i32;
                let row = Rect::new(px(8) as i32, y, bounds.w - px(16), px(32));
                paint::text(ctx, s, Role::Body, left, paint::baseline(ctx, Role::Body, row), "Дата и время", p.ink3);
                rows.push((row.translate(ox, oy), Action::ClockSettings));
            }
        }
        self.rows = rows;
    }
}
