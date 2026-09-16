// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Полёт окна в стопку и обратно: анимация сворачивания («джинн»).
//!
//! # Как это выглядит
//!
//! Окно не уменьшается целиком, а **утекает** в цель: нижние строки сужаются к
//! ширине цели раньше верхних, и какое-то время окно — трапеция, горлышком
//! вниз, к стопке в доке. Разворачивание — то же самое в обратную сторону.
//! Так это устроено у macOS, и Роман прислал именно такой снимок.
//!
//! # Почему без плавающей точки
//!
//! Ядро работает без контекста FPU (см. `glyphicon`), поэтому время и доли —
//! в неподвижной точке: `ONE` = 1024 — это «целиком».
//!
//! # Во что обходится кадр
//!
//! Строка назначения берёт строку поверхности окна и растягивает её до своей
//! ширины выборкой ближайшей точки. Сглаживания нет намеренно: кадр живёт
//! шестнадцать миллисекунд, и сглаженная выборка удвоила бы цену того, что
//! глаз не успевает рассмотреть. Окно при этом не перерисовывается вовсе —
//! берётся готовая поверхность.

use mini_ui::{Rect, Surface};

use super::window::App;

/// «Целиком» в неподвижной точке.
const ONE: i64 = 1024;

/// Сколько длится полёт.
///
/// Треть секунды: меньше — глаз не прослеживает, куда ушло окно, и полёт
/// выглядит миганием; больше — человек ждёт анимацию, а не окно.
const DURATION_NS: u64 = 320_000_000;

/// Полёт одного окна.
pub struct Flight {
    pub app: App,
    /// Где окно стоит на экране (для разворачивания — куда вернётся).
    pub from: Rect,
    started_ns: u64,
    /// Разворачивание: полёт идёт от цели к окну.
    pub restore: bool,
    /// Что было нарисовано прошлым кадром — стереть это надо и в этом.
    pub last: Rect,
    /// Сколько кадров полёт занял — в журнал: полёт длится треть секунды, и
    /// снимок экрана стендом его не ловит, а число кадров говорит, был ли он.
    pub frames: u32,
}

impl Flight {
    #[must_use]
    pub fn new(app: App, from: Rect, restore: bool, now_ns: u64) -> Self {
        Self { app, from, started_ns: now_ns, restore, last: Rect::EMPTY, frames: 0 }
    }

    /// Доля пройденного, `0..=ONE`, уже с учётом направления: ноль — окно на
    /// своём месте, `ONE` — окно в цели.
    #[must_use]
    pub fn phase(&self, now_ns: u64) -> i64 {
        let elapsed = now_ns.saturating_sub(self.started_ns).min(DURATION_NS);
        let t = (elapsed as i64 * ONE) / DURATION_NS as i64;
        if self.restore { ONE - t } else { t }
    }

    /// Сколько миллисекунд прошло с начала.
    #[must_use]
    pub fn elapsed_ms(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.started_ns) / 1_000_000
    }

    #[must_use]
    pub fn done(&self, now_ns: u64) -> bool {
        now_ns.saturating_sub(self.started_ns) >= DURATION_NS
    }

    /// Прямоугольник, внутри которого окно лежит на этой доле пути.
    #[must_use]
    pub fn bounds(&self, to: Rect, t: i64) -> Rect {
        let (top, bottom) = vertical(self.from, to, t);
        let left = self.from.x.min(to.x);
        let right = self.from.right().max(to.right());
        Rect::new(left, top, (right - left).max(0) as u32, (bottom - top).max(0) as u32)
    }

    /// Нарисовать окно на этой доле пути в полосу кадра.
    ///
    /// `surface` — поверхность окна, `band` — полоса в координатах экрана,
    /// `dy` — сдвиг из экрана в полосу.
    pub fn draw(&self, back: &mut Surface, surface: &Surface, to: Rect, t: i64, band: Rect, dy: i32) {
        let from = self.from;
        let (top, bottom) = vertical(from, to, t);
        let height = (bottom - top) as i64;
        if height <= 0 || surface.width() == 0 || surface.height() == 0 {
            return;
        }
        let first = top.max(band.y);
        let last = bottom.min(band.bottom());
        let screen_w = back.width() as i32;
        for y in first..last {
            // Где эта строка внутри окна: сверху (0) вниз (ONE).
            let v = ((y - top) as i64 * ONE) / height;
            // Нижние строки сужаются раньше верхних — отсюда горлышко.
            let pinch = (t * 9 / 5 - (ONE - v) * 4 / 5).clamp(0, ONE);
            let e = ease(pinch);
            let left = lerp(from.x, to.x, e);
            let right = lerp(from.right(), to.right(), e);
            let span = right - left;
            if span <= 0 {
                continue;
            }
            let src_y = ((v * i64::from(surface.height())) / ONE).min(i64::from(surface.height()) - 1) as u32;
            let source = surface.row(src_y);
            let row = back.row_mut((y + dy) as u32);
            let x0 = left.max(band.x).max(0);
            let x1 = right.min(band.right()).min(screen_w);
            for x in x0..x1 {
                let src_x = (((x - left) as i64 * i64::from(surface.width())) / span as i64)
                    .min(i64::from(surface.width()) - 1) as usize;
                if let (Some(dst), Some(src)) = (row.get_mut(x as usize), source.get(src_x)) {
                    *dst = *src;
                }
            }
        }
    }
}

/// Верх и низ окна на этой доле пути. Низ приезжает раньше верха: горлышко
/// уже у цели, а окно ещё тянется за ним сверху.
fn vertical(from: Rect, to: Rect, t: i64) -> (i32, i32) {
    let top = lerp(from.y, to.y, ease(t));
    let bottom = lerp(from.bottom(), to.bottom(), ease((t * 7 / 5).min(ONE)));
    (top, bottom.max(top))
}

/// Плавный разгон и торможение (smoothstep) в неподвижной точке.
fn ease(x: i64) -> i64 {
    let x = x.clamp(0, ONE);
    x * x * (3 * ONE - 2 * x) / (ONE * ONE)
}

fn lerp(a: i32, b: i32, t: i64) -> i32 {
    (i64::from(a) + (i64::from(b) - i64::from(a)) * t / ONE) as i32
}
