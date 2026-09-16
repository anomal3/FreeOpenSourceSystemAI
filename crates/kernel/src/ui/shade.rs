// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Шторка телефона: то, что открывается сверху.
//!
//! # Что в ней есть
//!
//! Состояние машины и переключатели к нему: связь, отладка, яркость, звук,
//! уведомления. На настольной машине то же самое живёт в трее и в «Параметрах»,
//! и шторки там нет вовсе — она открывается жестом, которого мышью не сделать.
//!
//! # Почему переключатели ничего не переключают
//!
//! Потому что переключать пока нечего, и это сказано вслух прямо на плитке:
//! у Wi-Fi написано «драйвера нет», у Bluetooth — «выключен». Плитка, которая
//! нажимается и молча ничего не делает, хуже отсутствующей: человек решит, что
//! сломалось. Плитка, честно называющая своё состояние, — это список работ,
//! который видно с экрана.
//!
//! Яркость и звук показаны полосой без ручки по той же причине: панелью
//! управляет загрузчик, регулятора у нас нет, и ползунок, который не двигается,
//! означал бы поломку.
//!
//! # Как она устроена внутри
//!
//! Тем же способом, что меню запуска и меню стола: своя поверхность, своё место
//! на экране, свой прямоугольник изменений. Композитор кладёт её поверх окон,
//! срезает углы и рисует тень — шторка об этом не знает.

use alloc::format;

use mini_ui::paint::{self, Ctx};
use mini_ui::glyphicon::{self, Icon};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

/// Плитка состояния: значок, подпись, пояснение под ней.
struct Tile {
    icon: Icon,
    label: &'static str,
    note: &'static str,
    /// Работает ли то, о чём плитка. Пока не работает ничего — но цвет
    /// заведён сразу, чтобы включённая плитка не потребовала переделки.
    on: bool,
}

/// Плитки в том порядке, в каком они стоят в макете.
const TILES: [Tile; 4] = [
    Tile { icon: Icon::Network, label: "Wi-Fi", note: "драйвера нет", on: false },
    Tile { icon: Icon::Devices, label: "Bluetooth", note: "выключен", on: false },
    Tile { icon: Icon::Clock, label: "Не беспокоить", note: "выключено", on: false },
    Tile { icon: Icon::Log, label: "Отладка", note: "журнал по USB", on: true },
];

/// Сколько плиток в ряду.
const COLUMNS: u32 = 2;

/// Высота плитки.
const TILE_H: u32 = 64;

/// Высота строки с полосой (яркость, звук).
const SLIDER_H: u32 = 44;

/// Шторка: слой поверх стола.
pub struct Shade {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    open: bool,
    damage: Rect,
}

impl Shade {
    /// Собрать шторку под этот экран.
    ///
    /// `None` — не хватило памяти под поверхность. Это не отказ стола: шторки
    /// просто не будет, а всё остальное работает.
    #[must_use]
    pub fn new(screen_w: u32, work_bottom: i32, scale: u32) -> Option<Self> {
        if !theme::is_mobile() || work_bottom <= 0 {
            return None;
        }
        let scale = scale.max(1);
        let ctx = Ctx::scaled(scale);
        // Макет (экран 02): от верха экрана, с полями по 8, и строка времени —
        // своя, внутри шторки: она закрывает строку состояния, а не лежит под ней.
        let side = ctx.px(8);
        let width = screen_w.saturating_sub(side * 2).max(1);
        let height = ctx
            .px(14 + 30 + 16 + TILE_H * 2 + 9 + 16 + SLIDER_H * 2 + 10 + 16 + 10 + 16 + 64 + 16 + 4 + 18)
            .min(work_bottom.max(1) as u32);
        let surface = Surface::new(width, height, bg())?;
        Some(Self {
            surface,
            rect: Rect::new(side as i32, side as i32, width, height),
            scale,
            open: false,
            damage: Rect::EMPTY,
        })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Открыть или закрыть. Возвращает новое состояние.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        if self.open {
            self.redraw();
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Попадает ли точка экрана в шторку.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.open && self.rect.contains(x, y)
    }

    /// Забрать накопленные изменения.
    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Перерисовать содержимое.
    fn redraw(&mut self) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(38);

        // Заливка во всю поверхность без скругления: углы срезает композитор,
        // смешивая их с тем, что под ними. Скругление здесь означало бы, что
        // углы срезаются дважды.
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);

        let left = card.x + ctx.px(16) as i32;
        let room = card.w.saturating_sub(ctx.px(32));
        let mut y = card.y + ctx.px(14) as i32;

        // Строка времени. Даты у телефона нет — часов реального времени тоже,
        // — поэтому рядом со временем сказано, что это время работы.
        let head = Rect::new(left + ctx.px(6) as i32, y, room.saturating_sub(ctx.px(12)), ctx.px(30));
        let time = super::clock_or_uptime();
        let time_w = paint::text(ctx, &mut self.surface, Role::Mono, head.x, paint::baseline(ctx, Role::Mono, head), &time, p.ink);
        let note = if crate::time::clock_text().is_some() { "" } else { "время работы" };
        paint::text(ctx, &mut self.surface, Role::Label, head.x + (time_w + ctx.px(8)) as i32, paint::baseline(ctx, Role::Label, head), note, p.ink4);
        y += ctx.px(30 + 16) as i32;

        // Плитки: два ряда по две.
        let gap = ctx.px(9);
        let tile_w = (room.saturating_sub(gap * (COLUMNS - 1))) / COLUMNS;
        let tile_h = ctx.px(TILE_H);
        for (index, tile) in TILES.iter().enumerate() {
            let column = index as u32 % COLUMNS;
            let row = index as u32 / COLUMNS;
            let rect = Rect::new(
                left + (column * (tile_w + gap)) as i32,
                y + (row * (tile_h + gap)) as i32,
                tile_w,
                tile_h,
            );
            self.draw_tile(ctx, rect, tile);
        }
        y += (tile_h * 2 + gap) as i32 + ctx.px(16) as i32;

        // Полосы: яркость и звук. Без ручки — двигать их нечем.
        for (label, icon, percent, accent) in [("Яркость", Icon::Sun, 72u32, true), ("Звук", Icon::Info, 30, false)] {
            let rect = Rect::new(left, y, room, ctx.px(SLIDER_H));
            self.draw_slider(ctx, rect, label, icon, percent, accent);
            y += ctx.px(SLIDER_H + 10) as i32;
        }
        y += ctx.px(6) as i32;

        // Уведомлений у системы пока нет вовсе, и это сказано прямо. Пустой
        // раздел без строки читался бы как не дорисованный экран.
        paint::caps(ctx, &mut self.surface, left + ctx.px(6) as i32, y, "УВЕДОМЛЕНИЯ · 0");
        y += ctx.px(10 + 16) as i32;
        let empty = Rect::new(left, y, room, ctx.px(64));
        draw::rounded(&mut self.surface, empty, ctx.px(20), p.card.color, p.card.alpha);
        draw::rounded_stroke(&mut self.surface, empty, ctx.px(20), p.line2.color, p.line2.alpha);
        paint::text_clipped(ctx, &mut self.surface, Role::Body, empty.x + ctx.px(14) as i32, paint::baseline(ctx, Role::Body, empty), empty.w, "Уведомлений пока нет", p.ink3);
        y += ctx.px(64 + 16) as i32;

        let handle = Rect::new(card.w as i32 / 2 - ctx.px(22) as i32, y, ctx.px(44), ctx.px(4));
        draw::rounded(&mut self.surface, handle, handle.h / 2, p.ink6, 180);

        self.damage = card;
    }

    /// Одна плитка состояния: значок слева, имя и состояние справа. Включённая
    /// залита акцентом.
    fn draw_tile(&mut self, ctx: Ctx, rect: Rect, tile: &Tile) {
        let p = ctx.palette;
        let round = ctx.px(20);
        let ink = if tile.on {
            draw::rounded_gradient(&mut self.surface, rect, round, p.acc, p.acc2, 255);
            draw::rounded_stroke(&mut self.surface, rect, round, p.accline.color, p.accline.alpha);
            Color::rgb(0xFF, 0xFF, 0xFF)
        } else {
            draw::rounded(&mut self.surface, rect, round, p.btn.color, p.btn.alpha);
            draw::rounded_stroke(&mut self.surface, rect, round, p.btnline.color, p.btnline.alpha);
            p.ink2
        };
        let glyph = ctx.px(20);
        let icon_x = rect.x + ctx.px(14) as i32;
        glyphicon::draw(&mut self.surface, tile.icon, icon_x, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, ink, 255);
        let text_x = icon_x + (glyph + ctx.px(11)) as i32;
        let room = (rect.right() - ctx.px(10) as i32 - text_x).max(0) as u32;
        let title = ctx.face(Role::Title);
        let small = ctx.face(Role::MonoSmall);
        let top = rect.y + (rect.h as i32 - i32::from(title.line) - i32::from(small.line)) / 2;
        paint::text_clipped(ctx, &mut self.surface, Role::Title, text_x, top, room, tile.label, ink);
        let note_ink = if tile.on { ink } else { p.ink4 };
        paint::text_clipped(ctx, &mut self.surface, Role::MonoSmall, text_x, top + i32::from(title.line), room, tile.note, note_ink);
    }

    /// Полоса: кнопка во всю ширину, внутри — заливка на долю и подпись.
    fn draw_slider(&mut self, ctx: Ctx, rect: Rect, label: &str, icon: Icon, percent: u32, accent: bool) {
        let p = ctx.palette;
        let round = ctx.px(16);
        draw::rounded(&mut self.surface, rect, round, p.btn.color, p.btn.alpha);
        let filled = rect.w * percent.min(100) / 100;
        if filled > 0 {
            let part = Rect::new(rect.x, rect.y, filled, rect.h);
            if accent {
                draw::horizontal_gradient(&mut self.surface, part, round, p.acc2, p.acc, 140);
            } else {
                draw::rounded(&mut self.surface, part, round, p.ink6, 115);
            }
        }
        draw::rounded_stroke(&mut self.surface, rect, round, p.btnline.color, p.btnline.alpha);
        let glyph = ctx.px(18);
        let icon_x = rect.x + ctx.px(16) as i32;
        glyphicon::draw(&mut self.surface, icon, icon_x, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, p.ink, 255);
        let text = format!("{label} {percent} %");
        let text_x = icon_x + (glyph + ctx.px(10)) as i32;
        paint::text_clipped(ctx, &mut self.surface, Role::Label, text_x, paint::baseline(ctx, Role::Label, rect), rect.w, &text, p.ink);
    }
}

/// Подложка шторки — та же, что у прочих плавающих слоёв стола.
fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}
