//! Всплывающие окна трея (фаза 72h-1, макет 05): раскладка, звук, сеть,
//! календарь, быстрые настройки с уведомлениями (колокол, 02.11).
//!
//! Чего у машины нет (Wi-Fi, Bluetooth, ночной свет, «В самолёте», яркость,
//! батарея), стоит серым: правило дома — нереализованное показывать, а не
//! прятать. Громкость — настоящая: отсчёты масштабирует `sound::feed`.
//!
//! Размеры из таблицы макета: скругление 8, поле 16, до панели 12; ячейка
//! календаря 34, сегодня — круг акцента.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::iconset::{self, Fi};
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
    Volume,
    Quick,
}

/// Что делает строка окна.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Layout(Layout),
    LanguageSettings,
    NetworkSettings,
    ClockSettings,
    /// Громкость (0..=100) — щелчком по ползунку.
    Volume(u8),
    /// Выключить звук или вернуть прежний.
    Mute,
    /// «Параметры» (шестерня быстрых настроек).
    Settings,
    /// «Очистить» историю уведомлений.
    ClearNotes,
}

/// Открытое окно трея.
pub struct TrayPop {
    pub kind: Kind,
    pub rect: Rect,
    surface: Surface,
    /// Строки и кнопки в координатах экрана.
    rows: Vec<(Rect, Action)>,
    /// Дорожка ползунка громкости в координатах экрана: щелчок — значение по x.
    slider: Option<Rect>,
}

const PAD: u32 = 16;
/// Ячейка календаря (ширина 340 по макету).
const CELL: u32 = 44;
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
            Kind::Layout => (px(180), px(PAD + 24 + 32 * 2 + 9 + 32 + 12)),
            Kind::Volume => (px(300), px(PAD + 28 + 8 + 36 + 22 + PAD)),
            Kind::Quick => {
                // Уведомлений — до трёх строк по 44, иначе одна «нет новых».
                let notes = super::toast::notes().len().clamp(1, 3) as u32;
                (px(360), px(PAD + 28 + notes * 44 + 12 + 28 + 64 * 2 + 8 + 12 + 36 * 2 + 12 + 40 + 8))
            }
            Kind::Network => (px(320), px(PAD + 28 + 8 + 52 + 16 + 32 + PAD)),
            Kind::Calendar => (px(CELL * 7 + PAD * 2), px(PAD + 28 + 18 + 12 + 24 + CELL_H * 6 + 8 + 32 + 12)),
        };
        let x = (anchor.x + anchor.w as i32 / 2 - w as i32 / 2).clamp(px(12) as i32, (screen_w as i32 - w as i32 - px(12) as i32).max(0));
        let y = panel_top - px(12) as i32 - h as i32;
        let p = ctx.palette;
        let fill = p.tb1.over(p.tb2.color);
        let surface = Surface::new(w, h, fill)?;
        let mut pop = Self { kind, rect: Rect::new(x, y, w, h), surface, rows: Vec::new(), slider: None };
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
        if let Some(track) = self.slider {
            // Ползунок ловит щелчок с запасом по высоте: дорожка в 4 точки.
            let zone = Rect::new(track.x - 8, track.y - 12, track.w + 16, track.h + 24);
            if zone.contains(x, y) {
                let value = ((x - track.x).clamp(0, track.w as i32) * 100 / track.w.max(1) as i32) as u8;
                return Some(Action::Volume(value));
            }
        }
        self.rows.iter().find(|(rect, _)| rect.contains(x, y)).map(|(_, action)| *action)
    }

    /// Громкость под точкой `x` экрана — для ползунка, который тянут.
    #[must_use]
    pub fn volume_at(&self, x: i32) -> Option<u8> {
        let track = self.slider?;
        Some(((x - track.x).clamp(0, track.w as i32) * 100 / track.w.max(1) as i32) as u8)
    }

    /// Перерисовать с текущими значениями (громкость сменилась).
    pub fn refresh(&mut self, scale: u32) {
        self.redraw(scale);
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
        let mut slider = None;
        let s = &mut self.surface;
        let left = px(PAD) as i32;
        let inner_w = bounds.w - px(PAD * 2);
        match self.kind {
            Kind::Volume => {
                let mut y = px(PAD) as i32;
                let head = Rect::new(left, y, inner_w, px(28));
                paint::text(ctx, s, Role::Title, left, paint::baseline(ctx, Role::Title, head), "Звук", p.ink);
                y += px(28 + 8) as i32;
                let row = Rect::new(left, y, inner_w, px(36));
                let present = crate::sound::present();
                let track = volume_row(ctx, s, row, crate::sound::volume(), present, Fi::Volume);
                if present {
                    slider = Some(track.translate(ox, oy));
                    rows.push((Rect::new(row.x, row.y, px(24), row.h).translate(ox, oy), Action::Mute));
                }
                let note = if present { "Динамики (встроенные)" } else { "Звуковой карты нет" };
                paint::text(ctx, s, Role::Caption, left + px(32) as i32, row.bottom() + px(2) as i32, note, p.ink4);
            }
            Kind::Quick => {
                let mut y = px(PAD) as i32;
                // Уведомления — сверху: колокол открывает их вместе с
                // быстрыми настройками.
                let head = Rect::new(left, y, inner_w, px(28));
                paint::text(ctx, s, Role::Title, left, paint::baseline(ctx, Role::Title, head), "Уведомления", p.ink);
                y += px(28) as i32;
                let notes = super::toast::notes();
                if notes.is_empty() {
                    let empty = Rect::new(left, y, inner_w, px(44));
                    paint::text(ctx, s, Role::Body, left, paint::baseline(ctx, Role::Body, empty), "Нет новых уведомлений", p.ink4);
                    y += px(44) as i32;
                } else {
                    // «Очистить» — справа от заголовка.
                    let label = "Очистить";
                    let cw = ctx.face(Role::Caption).width(label) + px(12);
                    let clear = Rect::new(bounds.right() - px(PAD) as i32 - cw as i32, px(PAD) as i32, cw, px(28));
                    paint::text_right(ctx, s, Role::Caption, clear.right(), paint::baseline(ctx, Role::Caption, clear), label, p.acc_ink);
                    rows.push((clear.translate(ox, oy), Action::ClearNotes));
                    for note in notes.iter().take(3) {
                        let row = Rect::new(left, y, inner_w, px(44));
                        iconset::draw(s, Fi::Bell, left, row.y + px(4) as i32, px(16), p.ink3, 255);
                        let tx = left + px(26) as i32;
                        let room = (row.right() - tx).max(0) as u32;
                        paint::text_clipped(ctx, s, Role::Body, tx, row.y + px(2) as i32, room.saturating_sub(px(80)), &note.title, p.ink);
                        paint::text_right(ctx, s, Role::Caption, row.right(), row.y + px(4) as i32, &super::toast::ago(note.at_ms), p.ink4);
                        paint::text_clipped(ctx, s, Role::Caption, tx, row.y + px(22) as i32, room, &note.text, p.ink3);
                        y += px(44) as i32;
                    }
                }
                y += px(12) as i32;
                draw::hline(s, left, y - px(6) as i32, inner_w, p.line2.color, p.line2.alpha);
                let head = Rect::new(left, y, inner_w, px(28));
                paint::text(ctx, s, Role::Strong, left, paint::baseline(ctx, Role::Strong, head), "Быстрые настройки", p.ink2);
                y += px(28) as i32;
                // Плитки 3×2 по 64: у машины нет ни одной из этих служб —
                // все серые (правило дома).
                let tiles = [
                    (Fi::Wifi, "Wi-Fi"),
                    (Fi::Bluetooth, "Bluetooth"),
                    (Fi::DoNotDisturb, "Не беспокоить"),
                    (Fi::NightLight, "Ночной свет"),
                    (Fi::Airplane, "В самолёте"),
                    (Fi::BatterySaver, "Экономия"),
                ];
                let gap = px(8);
                let tile_w = (inner_w - gap * 2) / 3;
                for (index, (icon, label)) in tiles.iter().enumerate() {
                    let tile = Rect::new(left + ((index as u32 % 3) * (tile_w + gap)) as i32, y + ((index as u32 / 3) * (px(64) + gap)) as i32, tile_w, px(64));
                    draw::rounded(s, tile, px(6), p.btn.over(fill), 255);
                    draw::rounded_stroke(s, tile, px(6), p.btnline.color, p.btnline.alpha);
                    let g = px(18);
                    iconset::draw(s, *icon, tile.x + px(10) as i32, tile.y + px(10) as i32, g, p.ink5, 255);
                    paint::text_clipped(ctx, s, Role::Label, tile.x + px(10) as i32, tile.bottom() - px(24) as i32, tile.w - px(14), label, p.ink5);
                }
                y += (px(64) * 2 + gap + px(12)) as i32;
                // Яркость — серой (нет драйвера подсветки), звук — настоящий.
                let row = Rect::new(left, y, inner_w, px(36));
                volume_row(ctx, s, row, 70, false, Fi::Brightness);
                y += px(36) as i32;
                let row = Rect::new(left, y, inner_w, px(36));
                let present = crate::sound::present();
                let track = volume_row(ctx, s, row, crate::sound::volume(), present, Fi::Volume);
                if present {
                    slider = Some(track.translate(ox, oy));
                }
                y += px(36 + 12) as i32;
                draw::hline(s, left, y - px(6) as i32, inner_w, p.line2.color, p.line2.alpha);
                // Нижняя строка: питание и шестерня «Параметров».
                let bottom = Rect::new(left, y, inner_w, px(40));
                let g = px(18);
                iconset::draw(s, Fi::Battery, left, bottom.y + (bottom.h as i32 - g as i32) / 2, g, p.ink5, 255);
                paint::text(ctx, s, Role::Caption, left + px(28) as i32, paint::baseline(ctx, Role::Caption, bottom), "Питание от сети · батареи нет", p.ink4);
                let gear = Rect::new(bottom.right() - px(32) as i32, bottom.y + px(4) as i32, px(32), px(32));
                glyphicon::draw(s, Icon::Settings, gear.x + px(7) as i32, gear.y + px(7) as i32, px(18), p.ink2, 255);
                rows.push((gear.translate(ox, oy), Action::Settings));
            }
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
        self.slider = slider;
    }
}

/// Строка с ползунком (макет 05): значок, дорожка 4, ручка 20, число справа.
/// `enabled: false` — серая, без ручки. Ответ — дорожка (координаты окна).
fn volume_row(ctx: Ctx, s: &mut Surface, row: Rect, value: u8, enabled: bool, icon: Fi) -> Rect {
    let p = ctx.palette;
    let px = |v: u32| ctx.px(v);
    let g = px(18);
    let ink = if enabled { p.ink2 } else { p.ink5 };
    let icon = if enabled && value == 0 && icon == Fi::Volume { Fi::VolumeOff } else { icon };
    iconset::draw(s, icon, row.x, row.y + (row.h as i32 - g as i32) / 2, g, ink, 255);
    let track = Rect::new(row.x + px(32) as i32, row.y + (row.h as i32 - px(4) as i32) / 2, row.w.saturating_sub(px(32 + 44)), px(4));
    draw::rounded(s, track, px(2), p.line.color, p.line.alpha);
    let fill = track.w * u32::from(value.min(100)) / 100;
    if enabled {
        if fill > 0 {
            draw::rounded(s, Rect::new(track.x, track.y, fill, track.h), px(2), p.acc, 255);
        }
        let knob = px(20);
        let kx = track.x + fill as i32 - knob as i32 / 2;
        draw::rounded(s, Rect::new(kx, row.y + (row.h as i32 - knob as i32) / 2, knob, knob), knob / 2, Color::rgb(0xFF, 0xFF, 0xFF), 255);
        draw::rounded(s, Rect::new(kx + px(6) as i32, row.y + (row.h as i32 - px(8) as i32) / 2, px(8), px(8)), px(4), p.acc, 255);
    }
    let text = alloc::format!("{value}");
    paint::text_right(ctx, s, Role::Body, row.right(), paint::baseline(ctx, Role::Body, row), &text, ink);
    track
}
