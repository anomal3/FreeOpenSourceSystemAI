// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Строка состояния наверху экрана телефона.
//!
//! # Почему она есть только на телефоне
//!
//! На столе то же самое показывает панель задач внизу, и вторая строка с теми
//! же числами была бы просто потерянной полосой экрана. У телефона панель внизу
//! — это док с приложениями, места под часы и память в нём нет, а верх экрана и
//! так занят вырезом камеры: строка состояния ложится по обе стороны от него,
//! то есть на место, которое всё равно ничем не занять.
//!
//! # Почему она рисуется со столом, а не окном
//!
//! Потому что она часть стола и лежит **под** окнами: слой, собранный
//! композитором, перекрывает её сам, и обрезать себя ей не нужно. Окном она
//! требовала бы поверхности, места в списке окон и попадания указателя — всего
//! того, что для полосы текста на обоях не нужно вовсе.
//!
//! # Чего она не спрашивает и почему
//!
//! Состояния сети. Рисование идёт под замком стола, а у сети свой замок, и
//! встреча двух задач на нём с выключенными прерываниями останавливает машину —
//! это уже случалось (см. [`super::net_state`]). Числа памяти берутся из
//! атомиков, которые обновляет [`super::status_now`] снаружи замка.

use alloc::format;
use alloc::string::String;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

/// Место, которое строка занимает наверху экрана.
#[must_use]
pub fn bounds(screen_w: u32, scale: u32) -> Rect {
    Rect::new(0, 0, screen_w, Ctx::scaled(scale).px(theme::M_STATUS_H))
}

/// Нарисовать строку состояния в полосу кадра (макет, экран 01).
///
/// Слева часы, справа галочка шторки, уровень сети и батарея. Числа — из
/// макета в его точках (строка 46, поля 24, промежутки 9).
///
/// # Что значки говорят на самом деле
///
/// Правду, а не картинку из макета. Модема и драйвера Wi-Fi у нас нет — все
/// четыре полоски сети приглушены. Заряда ядро не знает (у телефона он за
/// контроллером питания, с которым мы не говорим) — батарея нарисована пустым
/// контуром и без процентов. Появится источник — появится заливка; выдумывать
/// «62 %» значило бы показывать человеку число, которому нельзя верить.
///
/// `dy` — сдвиг из координат экрана в координаты полосы, тот же, что у значков.
pub fn draw(back: &mut Surface, band: Rect, dy: i32, screen_w: u32, scale: u32) {
    if !theme::is_mobile() {
        return;
    }
    let bar = bounds(screen_w, scale);
    if bar.intersect(&band).is_empty() {
        return;
    }

    // Подложка — усреднённые обои, как у значков: строка лежит на них, и всё
    // полупрозрачное обязано сводиться поверх них, а не поверх окна.
    let palette = theme::palette();
    let ctx = Ctx::scaled(scale).on(theme::wall_average(palette));
    let p = ctx.palette;
    let mid = bar.y + dy + bar.h as i32 / 2;
    let pad = ctx.px(24) as i32;

    // Слева — часы. Их может не быть вовсе: машина без часов реального времени
    // знает только, сколько она работает, и это честнее пустого места.
    let face = ctx.face(Role::Mono);
    let left = super::clock_or_uptime();
    let y = mid - i32::from(face.line) / 2;
    paint::text_clipped(ctx, back, Role::Mono, pad, y, screen_w / 2, &left, p.ink);

    // Справа налево: батарея, сеть, галочка.
    let mut x = screen_w as i32 - pad;

    // Батарея 24×13 со скруглением 4 и носиком 2×5.
    let nub = Rect::new(x - ctx.px(2) as i32, mid - ctx.px(5) as i32 / 2, ctx.px(2), ctx.px(5));
    draw::rounded(back, nub, ctx.px(1), p.ink, 150);
    x = nub.x - ctx.px(2) as i32;
    let body = Rect::new(x - ctx.px(24) as i32, mid - ctx.px(13) as i32 / 2, ctx.px(24), ctx.px(13));
    // Контур в полторы точки макета — два кольца по точке при масштабе 2.
    let radius = ctx.px(4);
    for ring in 0..(ctx.px(3) / 2).max(1) {
        let inset = Rect::new(body.x + ring as i32, body.y + ring as i32, body.w - ring * 2, body.h - ring * 2);
        draw::rounded_stroke(back, inset, radius.saturating_sub(ring), p.ink, 255);
    }
    x = body.x - ctx.px(9) as i32;
    // С 03.11 у телефона есть показания (`crate::battery`): заливка по
    // заряду и число слева. Зелёная — на зарядке, красная — ниже 15 %.
    if let Some(state) = crate::battery::get() {
        let gap = ctx.px(3);
        let inner_w = body.w.saturating_sub(gap * 2);
        let fill_w = (inner_w * u32::from(state.percent.min(100)) / 100).max(ctx.px(1));
        let fill = Rect::new(body.x + gap as i32, body.y + gap as i32, fill_w, body.h.saturating_sub(gap * 2));
        let color = if state.charger { p.ok } else if state.percent < 15 { p.bad } else { p.ink };
        draw::rounded(back, fill, ctx.px(1), color, 255);
        // На зарядке — молния поверх заливки (03.11, Роман: «на батарее
        // должен быть значок, заряжается или нет»).
        if state.charger {
            let (cx, cy) = (body.x + body.w as i32 / 2, body.y + body.h as i32 / 2);
            let h = (body.h as i32 * 3) / 8;
            let w = ctx.px(1).max(1);
            let dark = Color::rgb(0x10, 0x18, 0x28);
            draw::line(back, (cx + h / 2, cy - h), (cx - h / 3, cy + h / 6), w + 1, dark, 255);
            draw::line(back, (cx - h / 3, cy + h / 6), (cx + h / 3, cy - h / 6), w + 1, dark, 255);
            draw::line(back, (cx + h / 3, cy - h / 6), (cx - h / 2, cy + h), w + 1, dark, 255);
        }
        let text = alloc::format!("{}%", state.percent);
        let w = face.width(&text);
        x -= w as i32;
        paint::text(ctx, back, Role::Mono, x, y, &text, p.ink);
        x -= ctx.px(9) as i32;
    }
    // Wi-Fi включён — его значок (веер), левее батареи. Тусклый: к сети мы
    // ещё не подключаемся (Роман 03.11: «значок появился, хотя Wi-Fi не
    // подключён, а просто включён»); ярким он станет с подключением.
    if matches!(crate::wifi::state(), crate::wifi::State::Ready | crate::wifi::State::Scanning) {
        let g = ctx.px(18);
        x -= g as i32;
        mini_ui::iconset::draw(back, mini_ui::iconset::Fi::Wifi, x, mid - g as i32 / 2, g, p.ink, 90);
        x -= ctx.px(9) as i32;
    }

    // Сеть: четыре полоски 3 шириной, высотой 5, 8, 11 и 14, по низу.
    let bottom = mid + ctx.px(14) as i32 / 2;
    let heights = [5u32, 8, 11, 14];
    let step = ctx.px(3 + 2) as i32;
    let bars_w = step * 4 - ctx.px(2) as i32;
    let bars_x = x - bars_w;
    for (index, h) in heights.iter().enumerate() {
        let rect = Rect::new(
            bars_x + step * index as i32,
            bottom - ctx.px(*h) as i32,
            ctx.px(3),
            ctx.px(*h),
        );
        draw::rounded(back, rect, ctx.px(1), p.ink, 77);
    }
    x = bars_x - ctx.px(9) as i32;

    // Галочка вверх: здесь открывается шторка (нажатием на строку).
    let chevron = ctx.px(15);
    glyphicon::draw(back, Icon::ChevronUp, x - chevron as i32, mid - chevron as i32 / 2, chevron, p.ink, 255);
}

/// Время работы словами — запасной вариант, когда часов нет.
#[must_use]
pub fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 3600, (seconds / 60) % 60)
}
