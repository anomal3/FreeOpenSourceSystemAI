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
        let inset = ctx.px(theme::M_INSET);
        let top = ctx.px(theme::M_STATUS_H);
        let width = screen_w.saturating_sub(inset * 2).max(1);
        // Высота — по содержимому: заголовок, два ряда плиток, две полосы,
        // заголовок уведомлений и строка «их нет». До низа экрана шторка не
        // доходит намеренно: под ней должен остаться виден стол, иначе её
        // нельзя закрыть нажатием мимо.
        let height = ctx
            .px(theme::M_INSET * 2 + 26 + TILE_H * 2 + 10 + SLIDER_H * 2 + 26 + 34)
            .min((work_bottom - top as i32).max(1) as u32);
        let surface = Surface::new(width, height, bg())?;
        Some(Self {
            surface,
            rect: Rect::new(inset as i32, top as i32, width, height),
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
        let m = ctx.px(theme::M_INSET);
        let round = ctx.px(theme::M_R_CARD);

        // Заливка во всю поверхность без скругления: углы срезает композитор,
        // смешивая их с тем, что под ними. Скругление здесь означало бы, что
        // углы срезаются дважды.
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);

        let mut y = card.y + m as i32;
        let left = card.x + m as i32;
        let room = card.w.saturating_sub(m * 2);

        // Дата вместо часов: часы стоят в строке состояния над шторкой, и
        // повторять их здесь незачем. Часов реального времени у телефона нет,
        // поэтому говорится то, что известно, — сколько машина работает.
        let head = super::clock_or_uptime();
        paint::text_clipped(ctx, &mut self.surface, Role::Title, left, y, room, &head, p.ink);
        y += i32::from(ctx.face(Role::Title).line) + ctx.px(10) as i32;

        // Плитки: два ряда по две.
        let gap = ctx.px(8);
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
        y += ((tile_h + gap) * 2) as i32;

        // Полосы: яркость и звук. Без ручки — двигать их нечем.
        for (label, percent) in [("Яркость", 72u32), ("Звук", 30)] {
            let rect = Rect::new(left, y, room, ctx.px(SLIDER_H));
            self.draw_slider(ctx, rect, label, percent);
            y += ctx.px(SLIDER_H + 6) as i32;
        }

        // Уведомлений у системы пока нет вовсе, и это сказано прямо. Пустой
        // раздел без строки читался бы как не дорисованный экран.
        y += ctx.px(4) as i32;
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::MonoCaps,
            left,
            y,
            room,
            "УВЕДОМЛЕНИЯ",
            p.ink5,
        );
        y += i32::from(ctx.face(Role::MonoCaps).line) + ctx.px(8) as i32;
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::Body,
            left,
            y,
            room,
            "их пока нет",
            p.ink3,
        );

        self.damage = card;
    }

    /// Одна плитка состояния.
    fn draw_tile(&mut self, ctx: Ctx, rect: Rect, tile: &Tile) {
        let p = ctx.palette;
        let round = ctx.px(theme::M_R_TILE);
        let (fill, alpha) = if tile.on {
            (p.acctint.color, p.acctint.alpha)
        } else {
            (p.card.color, p.card.alpha)
        };
        draw::rounded(&mut self.surface, rect, round, fill, alpha);
        draw::rounded_stroke(&mut self.surface, rect, round, p.line2.color, p.line2.alpha);

        let glyph = ctx.px(20);
        let pad = ctx.px(12);
        let tone = if tile.on { p.acc } else { p.ink3 };
        glyphicon::draw(
            &mut self.surface,
            tile.icon,
            rect.x + pad as i32,
            rect.y + pad as i32,
            glyph,
            tone,
            255,
        );
        let text_x = rect.x + pad as i32;
        let room = rect.w.saturating_sub(pad * 2);
        let mut y = rect.y + (pad + glyph) as i32 + ctx.px(6) as i32;
        paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, y, room, tile.label, p.ink);
        y += i32::from(ctx.face(Role::Body).line);
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::Caption,
            text_x,
            y,
            room,
            tile.note,
            p.ink5,
        );
    }

    /// Строка с полосой: подпись, доля, сама полоса.
    fn draw_slider(&mut self, ctx: Ctx, rect: Rect, label: &str, percent: u32) {
        let p = ctx.palette;
        let text = format!("{label} {percent} %");
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::Body,
            rect.x,
            rect.y,
            rect.w,
            &text,
            p.ink2,
        );
        let bar_h = ctx.px(6);
        let bar = Rect::new(
            rect.x,
            rect.bottom() - bar_h as i32 - ctx.px(6) as i32,
            rect.w,
            bar_h,
        );
        draw::rounded(&mut self.surface, bar, bar_h / 2, p.sunk.color, p.sunk.alpha);
        let filled = bar.w * percent.min(100) / 100;
        if filled > 0 {
            let done = Rect::new(bar.x, bar.y, filled, bar_h);
            draw::rounded_gradient(&mut self.surface, done, bar_h / 2, p.acc2, p.acc, 255);
        }
    }
}

/// Подложка шторки — та же, что у прочих плавающих слоёв стола.
fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}
