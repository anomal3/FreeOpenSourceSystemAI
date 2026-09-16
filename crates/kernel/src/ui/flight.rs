// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Полёт окна в стопку и обратно: анимация сворачивания («джинн»), — и
//! открытие и закрытие окна на телефоне («рост из плитки»).
//!
//! # Открытие и закрытие
//!
//! Окно вырастает из плитки своей программы на домашнем экране и уходит в неё
//! обратно, проявляясь и тая по дороге. Плитки нет — программу запустили из
//! «Пуска» или терминала — окно растёт из середины своего же места, с
//! четырёх пятых размера. Полёт тот же, что у джинна: готовая поверхность
//! окна растягивается выборкой ближайшей точки, перерисовки нет.
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

/// Сколько длится открытие и закрытие: короче сворачивания — человек ждёт
/// окно, а не смотрит, куда оно делось.
const ZOOM_NS: u64 = 240_000_000;

/// Как летит окно.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Сворачивание в стопку и обратно.
    Genie,
    /// Открытие (`restore`) и закрытие окна: рост из плитки и уход в неё.
    Zoom,
}

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
    pub motion: Motion,
    /// Цель, известная заранее, — плитка программы для открытия и закрытия.
    /// Цель джинна, стопка, едет вместе с доком и спрашивается каждый кадр.
    pub to: Option<Rect>,
}

impl Flight {
    #[must_use]
    pub fn new(app: App, from: Rect, restore: bool, now_ns: u64) -> Self {
        Self { app, from, started_ns: now_ns, restore, last: Rect::EMPTY, frames: 0, motion: Motion::Genie, to: None }
    }

    /// Открытие (`restore`) или закрытие окна — рост из `to` и уход в него.
    #[must_use]
    pub fn zoom(app: App, from: Rect, to: Rect, restore: bool, now_ns: u64) -> Self {
        Self { motion: Motion::Zoom, to: Some(to), ..Self::new(app, from, restore, now_ns) }
    }

    const fn duration_ns(&self) -> u64 {
        match self.motion {
            Motion::Genie => DURATION_NS,
            Motion::Zoom => ZOOM_NS,
        }
    }

    /// Доля пройденного, `0..=ONE`, уже с учётом направления: ноль — окно на
    /// своём месте, `ONE` — окно в цели.
    #[must_use]
    pub fn phase(&self, now_ns: u64) -> i64 {
        let duration = self.duration_ns();
        let elapsed = now_ns.saturating_sub(self.started_ns).min(duration);
        let t = (elapsed as i64 * ONE) / duration as i64;
        if self.restore { ONE - t } else { t }
    }

    /// Сколько миллисекунд прошло с начала.
    #[must_use]
    pub fn elapsed_ms(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.started_ns) / 1_000_000
    }

    #[must_use]
    pub fn done(&self, now_ns: u64) -> bool {
        now_ns.saturating_sub(self.started_ns) >= self.duration_ns()
    }

    /// Прямоугольник, внутри которого окно лежит на этой доле пути.
    #[must_use]
    pub fn bounds(&self, to: Rect, t: i64) -> Rect {
        if self.motion == Motion::Zoom {
            return zoom_rect(self.from, to, t);
        }
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
        if self.motion == Motion::Zoom {
            draw_zoom(back, surface, zoom_rect(self.from, to, t), fade(t), band, dy);
            return;
        }
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

/// Где окно на этой доле пути роста: прямоугольник между местом и плиткой.
fn zoom_rect(from: Rect, to: Rect, t: i64) -> Rect {
    let e = ease(t);
    let left = lerp(from.x, to.x, e);
    let top = lerp(from.y, to.y, e);
    let right = lerp(from.right(), to.right(), e);
    let bottom = lerp(from.bottom(), to.bottom(), e);
    Rect::new(left, top, (right - left).max(0) as u32, (bottom - top).max(0) as u32)
}

/// Непрозрачность окна, `0..=256`: первую половину пути от места оно не
/// тает вовсе, вторую — тает до нуля. У плитки окна уже не видно, и она
/// показывается из-под него, а не заслоняется точкой того же размера.
fn fade(t: i64) -> u32 {
    ((ONE - t) * 2 * 256 / ONE).clamp(0, 256) as u32
}

/// Растянуть поверхность окна в `rect` с непрозрачностью `alpha` из `0..=256`.
///
/// Шаг по источнику считается в неподвижной точке один раз на строку: деление
/// на каждой точке, как у джинна, стоило бы на полном экране миллион делений
/// за кадр. Смешиваются все четыре байта точки разом: у панели телефона альфа
/// обязана быть `0xff`, и смесь двух `0xff` остаётся `0xff`.
fn draw_zoom(back: &mut Surface, surface: &Surface, rect: Rect, alpha: u32, band: Rect, dy: i32) {
    if rect.is_empty() || alpha == 0 || surface.width() == 0 || surface.height() == 0 {
        return;
    }
    let first = rect.y.max(band.y);
    let last = rect.bottom().min(band.bottom());
    let x0 = rect.x.max(band.x).max(0);
    let x1 = rect.right().min(band.right()).min(back.width() as i32);
    if x0 >= x1 {
        return;
    }
    let step = (u64::from(surface.width()) << 16) / u64::from(rect.w);
    let start = (x0 - rect.x) as u64 * step;
    let keep = 256 - alpha;
    let max_x = surface.width() as usize - 1;
    for y in first..last {
        let src_y = (((y - rect.y) as u64 * u64::from(surface.height())) / u64::from(rect.h))
            .min(u64::from(surface.height()) - 1) as u32;
        let source = surface.row(src_y);
        let row = back.row_mut((y + dy) as u32);
        let (Some(dst), true) = (row.get_mut(x0 as usize..x1 as usize), source.len() > max_x) else {
            continue;
        };
        let mut acc = start;
        for pixel in dst.iter_mut() {
            let src = source[((acc >> 16) as usize).min(max_x)];
            acc += step;
            *pixel = if keep == 0 { src } else { mix(src, *pixel, alpha, keep) };
        }
    }
}

/// Смесь двух точек по байтам: `a` из `256` — доля первой.
fn mix(src: u32, dst: u32, a: u32, keep: u32) -> u32 {
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        out |= ((s * a + d * keep) >> 8) << shift;
    }
    out
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
