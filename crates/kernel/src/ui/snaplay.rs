//! Макеты прилипания (20.11, как Snap Layouts в Windows 11).
//!
//! Плашка с набором раскладок монитора: 50/50, 66/33, 33/66, три столбца,
//! половина и две четверти, 2×2, 25/50/25. Открывается двумя путями, как у
//! Windows: указатель полсекунды стоит на кнопке «Развернуть» — плашка под
//! ней в два ряда; окно тащат и держат у верхнего края монитора — плашка
//! вверху по центру в один ряд, и окно бросают прямо на зону. Щелчок (или
//! отпускание) по зоне ставит окно в эту часть рабочей области монитора.
//!
//! Своей поверхности у плашки нет: её рисует композитор прямо в полосу кадра,
//! как миниатюры, — она маленькая и живёт секунды.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::{Rect, Surface, draw, theme};

use super::window::App;

/// Раскладки: зоны в двенадцатых долях рабочей области (`x, y, w, h`).
pub const LAYOUTS: [&[(u8, u8, u8, u8)]; 7] = [
    &[(0, 0, 6, 12), (6, 0, 6, 12)],
    &[(0, 0, 8, 12), (8, 0, 4, 12)],
    &[(0, 0, 4, 12), (4, 0, 8, 12)],
    &[(0, 0, 4, 12), (4, 0, 4, 12), (8, 0, 4, 12)],
    &[(0, 0, 6, 12), (6, 0, 6, 6), (6, 6, 6, 6)],
    &[(0, 0, 6, 6), (6, 0, 6, 6), (0, 6, 6, 6), (6, 6, 6, 6)],
    &[(0, 0, 3, 12), (3, 0, 6, 12), (9, 0, 3, 12)],
];

/// Размеры плашки (точки при масштабе 1).
const GROUP_W: u32 = 84;
const GROUP_H: u32 = 54;
const GAP: u32 = 10;
const PAD: u32 = 10;
/// Зазор между зонами внутри раскладки.
const INNER: u32 = 3;

/// Открытая плашка.
pub struct Popup {
    /// Окно, которое ставится в зону.
    pub app: App,
    /// Где плашка на столе.
    pub rect: Rect,
    /// Рабочая область монитора, которую делят зоны.
    pub area: Rect,
    /// Зоны на плашке: раскладка, зона, место.
    pub zones: Vec<(usize, usize, Rect)>,
    /// Подложки раскладок.
    groups: Vec<Rect>,
    /// Зона под указателем.
    pub hot: Option<(usize, usize)>,
    /// Открыта перетаскиванием (у верхнего края), а не наведением на кнопку.
    pub dragging: bool,
    /// Кнопка «Развернуть», от которой плашка открыта: пока указатель на
    /// ней или на плашке, плашка не закрывается.
    pub button: Rect,
}

impl Popup {
    /// Плашка под точкой `x, y` (середина кнопки или указатель у верхнего
    /// края) в пределах монитора `monitor`; зоны делят `area`.
    #[must_use]
    pub fn new(app: App, x: i32, y: i32, monitor: Rect, area: Rect, dragging: bool, button: Rect, scale: mini_ui::Scale) -> Self {
        let px = |v: u32| v * scale;
        let count = LAYOUTS.len() as u32;
        let columns = if dragging { count } else { 4 };
        let rows = count.div_ceil(columns);
        let w = px(PAD) * 2 + columns * px(GROUP_W) + (columns - 1) * px(GAP);
        let h = px(PAD) * 2 + rows * px(GROUP_H) + (rows - 1) * px(GAP);
        let left = (x - w as i32 / 2).clamp(monitor.x + px(4) as i32, (monitor.right() - w as i32 - px(4) as i32).max(monitor.x));
        let top = y.clamp(monitor.y, (monitor.bottom() - h as i32).max(monitor.y));
        let rect = Rect::new(left, top, w, h);
        let mut groups = Vec::new();
        let mut zones = Vec::new();
        for (index, cells) in LAYOUTS.iter().enumerate() {
            let (col, row) = (index as u32 % columns, index as u32 / columns);
            let group = Rect::new(
                left + (px(PAD) + col * (px(GROUP_W) + px(GAP))) as i32,
                top + (px(PAD) + row * (px(GROUP_H) + px(GAP))) as i32,
                px(GROUP_W),
                px(GROUP_H),
            );
            groups.push(group);
            let inner = group.shrink(px(INNER));
            for (cell, part) in cells.iter().enumerate() {
                let mut zone = twelfths(inner, *part);
                // Зазор между соседними зонами — по половине с каждой стороны.
                let half = px(INNER).div_ceil(2);
                if part.0 > 0 {
                    zone = Rect::new(zone.x + half as i32, zone.y, zone.w.saturating_sub(half), zone.h);
                }
                if part.0 + part.2 < 12 {
                    zone.w = zone.w.saturating_sub(half);
                }
                if part.1 > 0 {
                    zone = Rect::new(zone.x, zone.y + half as i32, zone.w, zone.h.saturating_sub(half));
                }
                if part.1 + part.3 < 12 {
                    zone.h = zone.h.saturating_sub(half);
                }
                zones.push((index, cell, zone));
            }
        }
        Self { app, rect, area, zones, groups, hot: None, dragging, button }
    }

    /// Зона под точкой.
    #[must_use]
    pub fn zone_at(&self, x: i32, y: i32) -> Option<(usize, usize)> {
        self.zones.iter().find(|(_, _, rect)| rect.contains(x, y)).map(|(l, c, _)| (*l, *c))
    }

    /// Указатель сдвинулся: подсветить зону. `true` — подсветка сменилась.
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        let hot = self.zone_at(x, y);
        let changed = hot != self.hot;
        self.hot = hot;
        changed
    }

    /// Место окна для зоны `cell` раскладки `layout` — часть рабочей области.
    #[must_use]
    pub fn target(&self, layout: usize, cell: usize) -> Option<Rect> {
        let part = LAYOUTS.get(layout)?.get(cell)?;
        Some(twelfths(self.area, *part))
    }

    /// Держится ли плашка открытой при указателе в `x, y`: на ней, на её
    /// кнопке или в полосе между ними.
    #[must_use]
    pub fn keeps(&self, x: i32, y: i32, scale: mini_ui::Scale) -> bool {
        let margin = (12 * scale) as i32;
        let keep = if self.button.is_empty() { self.rect } else { self.rect.union(&self.button) };
        x >= keep.x - margin && x < keep.right() + margin && y >= keep.y - margin && y < keep.bottom() + margin
    }

    /// Места зон для стенда: `snap spot L.C at x,y wxh`.
    #[must_use]
    pub fn spots(&self) -> Vec<String> {
        self.zones
            .iter()
            .map(|(l, c, r)| alloc::format!("{}.{} at {},{} {}x{}", l + 1, c + 1, r.x, r.y, r.w, r.h))
            .collect()
    }

    /// Нарисовать плашку в полосу кадра.
    pub fn draw(&self, back: &mut Surface, band: Rect, dy: i32, scale: mini_ui::Scale) {
        if self.rect.intersect(&band).is_empty() {
            return;
        }
        let p = theme::palette();
        let px = |v: u32| v * scale;
        let fill = p.tb1.over(p.tb2.color);
        let plate = self.rect.translate(0, dy);
        draw::rounded(back, plate, px(theme::R_WINDOW), fill, 255);
        draw::rounded_stroke(back, plate, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let sunk = p.sunk.over(fill);
        for group in &self.groups {
            draw::rounded(back, group.translate(0, dy), px(6), sunk, 255);
        }
        for (layout, cell, zone) in &self.zones {
            let at = zone.translate(0, dy);
            if self.hot == Some((*layout, *cell)) {
                draw::rounded(back, at, px(4), p.acc, 255);
            } else {
                draw::rounded(back, at, px(4), p.btn.over(sunk), 255);
                draw::rounded_stroke(back, at, px(4), p.btnline.color, p.btnline.alpha);
            }
        }
    }
}

/// Часть `area` в двенадцатых долях.
fn twelfths(area: Rect, (x, y, w, h): (u8, u8, u8, u8)) -> Rect {
    let along = |start: i32, len: u32, from: u8, span: u8| {
        let a = start + (u64::from(len) * u64::from(from) / 12) as i32;
        let b = start + (u64::from(len) * u64::from(from + span) / 12) as i32;
        (a, (b - a).max(1) as u32)
    };
    let (rx, rw) = along(area.x, area.w, x, w);
    let (ry, rh) = along(area.y, area.h, y, h);
    Rect::new(rx, ry, rw, rh)
}
