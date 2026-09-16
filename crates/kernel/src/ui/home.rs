// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Домашний экран телефона (макет `FreeOS-mobile`, экран 01): карточка
//! «Система» над сеткой значков и карточка «Последнее» под ней.
//!
//! # Почему рисуется прямо в кадр, а не своей поверхностью
//!
//! Карточки полупрозрачные и лежат **на обоях**: цвет под каждой точкой в полосе
//! кадра уже известен, и стекло смешивается с ним честно. Своя поверхность
//! потребовала бы свести стекло заранее к среднему цвету обоев — и карточка
//! легла бы на градиент тёмным прямоугольником, ровно как было со значками.
//! Рисуются они только там, где их не закрывает окно (см. `compose_band`), то
//! есть тогда, когда их видно.
//!
//! # Что в карточках правда
//!
//! Всё. Память — из распределителя кадров, задачи — из планировщика, аптайм —
//! из часов, слот — из разметки, найденной при загрузке. У живого образа без
//! слотов вместо «СЛОТ A» написано «LIVE»: выдуманный слот на машине без слотов
//! означал бы, что обновление туда есть куда ставить.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use super::window::App;

/// Сведения для карточки «Система». Собираются вне замка стола (см.
/// `ui::status_now`): у планировщика и распределителя свои замки.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Facts {
    pub used_mib: u64,
    pub total_mib: u64,
    pub tasks: usize,
    /// `Some('A')`/`Some('B')` — загрузились со слота; `None` — живой образ.
    pub slot: Option<char>,
    /// Аптайм с точностью до секунды — чаще карточка не меняется.
    pub uptime_s: u64,
}

/// Числа макета, в его точках.
const TOP: u32 = 58;
const SIDE: u32 = 18;
const CARD_PAD: u32 = 18;
const GAP: u32 = 14;
const STAT_H: u32 = 73;
const SYSTEM_H: u32 = CARD_PAD * 2 + 10 + GAP + STAT_H + GAP + 18;
const RECENT_H: u32 = 14 * 2 + 10 + 10 + 48;

/// Где стоит карточка «Система».
#[must_use]
pub fn system_card(screen_w: u32, scale: u32) -> Rect {
    let ctx = Ctx::scaled(scale);
    Rect::new(
        ctx.px(SIDE) as i32,
        ctx.px(TOP) as i32,
        screen_w.saturating_sub(ctx.px(SIDE) * 2),
        ctx.px(SYSTEM_H),
    )
}

/// Верх сетки значков — под карточкой «Система».
#[must_use]
pub fn grid_top(scale: u32) -> u32 {
    Ctx::scaled(scale).px(TOP + SYSTEM_H + GAP)
}

/// Где стоит карточка «Последнее» — под сеткой, нижний край которой известен
/// только значкам.
#[must_use]
pub fn recent_card(screen_w: u32, scale: u32, grid_bottom: i32) -> Rect {
    let ctx = Ctx::scaled(scale);
    Rect::new(
        ctx.px(SIDE) as i32,
        grid_bottom + ctx.px(GAP) as i32,
        screen_w.saturating_sub(ctx.px(SIDE) * 2),
        ctx.px(RECENT_H),
    )
}

/// Стекло карточки: заливка, контур, корона.
fn glass(back: &mut Surface, ctx: Ctx, card: Rect, radius: u32) {
    let p = ctx.palette;
    draw::rounded(back, card, radius, p.win.color, p.win.alpha);
    draw::rounded_stroke(back, card, radius, p.line3.color, p.line3.alpha);
    draw::crown(back, card, radius, p.crown.color, p.crown.alpha);
}

/// Нарисовать карточку «Система» в полосу кадра.
pub fn draw_system(back: &mut Surface, band: Rect, dy: i32, screen_w: u32, scale: u32, facts: &Facts) {
    let screen_card = system_card(screen_w, scale);
    if screen_card.intersect(&band).is_empty() {
        return;
    }
    let card = screen_card.translate(0, dy);
    let ctx = Ctx::scaled(scale).on(theme::wall_average(theme::palette()));
    let p = ctx.palette;
    glass(back, ctx, card, ctx.px(24));

    let pad = ctx.px(CARD_PAD) as i32;
    let inner = Rect::new(card.x + pad, card.y + pad, card.w.saturating_sub(ctx.px(CARD_PAD) * 2), card.h);

    // Заголовок и слот.
    paint::caps(ctx, back, inner.x, inner.y, "СИСТЕМА");
    let slot = match facts.slot {
        Some(letter) => alloc::format!("СЛОТ {letter}"),
        None => String::from("LIVE"),
    };
    let mono = ctx.face(Role::MonoSmall);
    let badge_w = mono.width(&slot) + ctx.px(18);
    let badge = Rect::new(inner.right() - badge_w as i32, inner.y - ctx.px(4) as i32, badge_w, ctx.px(18));
    draw::rounded(back, badge, badge.h / 2, p.okbg.color, p.okbg.alpha);
    draw::rounded_stroke(back, badge, badge.h / 2, p.okline, 255);
    paint::text(ctx, back, Role::MonoSmall, badge.x + ctx.px(9) as i32, paint::baseline(ctx, Role::MonoSmall, badge), &slot, p.ok_ink);

    // Две плитки: память и задачи.
    let stats_y = inner.y + ctx.px(10 + GAP) as i32;
    let gap = ctx.px(10);
    let stat_w = inner.w.saturating_sub(gap) / 2;
    let used_pct = if facts.total_mib == 0 { 0 } else { (facts.used_mib * 100 / facts.total_mib) as u32 };
    // Шкала задач — доля от шестнадцати: число живых задач на этой системе
    // редко больше дюжины, и полоса, упёртая в край, говорила бы «перегружено».
    let tasks_pct = ((facts.tasks as u32) * 100 / 16).min(100);
    let stats = [
        ("ПАМЯТЬ", alloc::format!("{}", facts.used_mib), " МиБ", used_pct, Tone::Accent),
        ("ЗАДАЧИ", alloc::format!("{}", facts.tasks), " живых", tasks_pct, Tone::Ok),
    ];
    for (index, (label, value, unit, pct, tone)) in stats.iter().enumerate() {
        let tile = Rect::new(inner.x + (index as u32 * (stat_w + gap)) as i32, stats_y, stat_w, ctx.px(STAT_H));
        draw::rounded(back, tile, ctx.px(16), p.card.color, p.card.alpha);
        draw::rounded_stroke(back, tile, ctx.px(16), p.line2.color, p.line2.alpha);
        let x = tile.x + ctx.px(12) as i32;
        let mut y = tile.y + ctx.px(12) as i32;
        paint::caps(ctx, back, x, y, label);
        y += ctx.px(18) as i32;
        let value_w = paint::text(ctx, back, Role::Heading, x, y, value, p.ink);
        let heading = ctx.face(Role::Heading);
        let small = ctx.face(Role::MonoSmall);
        paint::text(
            ctx,
            back,
            Role::MonoSmall,
            x + value_w as i32,
            y + i32::from(heading.line) - i32::from(small.line) - ctx.px(2) as i32,
            unit,
            p.ink4,
        );
        let bar_y = tile.bottom() - ctx.px(12 + 5) as i32;
        let bar = Rect::new(x, bar_y, tile.w.saturating_sub(ctx.px(24)), ctx.px(5));
        draw::rounded(back, bar, bar.h / 2, p.sunk.color, p.sunk.alpha);
        let fill = Rect::new(bar.x, bar.y, bar.w * pct / 100, bar.h);
        if fill.w > 0 {
            match tone {
                Tone::Ok => draw::rounded(back, fill, fill.h / 2, p.ok, 255),
                _ => draw::horizontal_gradient(back, fill, fill.h / 2, p.acc2, p.acc, 255),
            }
        }
    }

    // Строка внизу: аптайм.
    let s = facts.uptime_s;
    let line = alloc::format!("Аптайм {}:{:02}", s / 3600, (s / 60) % 60);
    let line_y = stats_y + ctx.px(STAT_H + GAP) as i32;
    paint::text_clipped(ctx, back, Role::Body, inner.x, line_y, inner.w, &line, p.ink4);
}

/// Строка «Последнего»: программа, имя, что с ней.
pub struct Recent {
    pub app: App,
    pub title: String,
    pub note: String,
}

/// Нарисовать карточку «Последнее» в полосу кадра. Пустой список — карточки нет.
pub fn draw_recent(back: &mut Surface, band: Rect, dy: i32, screen_card: Rect, scale: u32, rows: &[Recent], background: usize) {
    if rows.is_empty() || screen_card.intersect(&band).is_empty() {
        return;
    }
    let card = screen_card.translate(0, dy);
    let ctx = Ctx::scaled(scale).on(theme::wall_average(theme::palette()));
    let p = ctx.palette;
    glass(back, ctx, card, ctx.px(22));

    let x = card.x + ctx.px(16) as i32;
    let w = card.w.saturating_sub(ctx.px(32));
    let y = card.y + ctx.px(14) as i32;
    paint::caps(ctx, back, x, y, "ПОСЛЕДНЕЕ");
    let count = alloc::format!("{background} {} в фоне", tasks_word(background));
    let mono = ctx.face(Role::MonoSmall);
    let count_w = mono.width(&count);
    paint::text(ctx, back, Role::MonoSmall, x + w as i32 - count_w as i32, y, &count, p.ink4);

    let chip_y = y + ctx.px(20) as i32;
    let gap = ctx.px(9);
    let chip_w = w.saturating_sub(gap) / 2;
    for (index, row) in rows.iter().take(2).enumerate() {
        let chip = Rect::new(x + (index as u32 * (chip_w + gap)) as i32, chip_y, chip_w, ctx.px(48));
        draw::rounded(back, chip, ctx.px(14), p.card.color, p.card.alpha);
        draw::rounded_stroke(back, chip, ctx.px(14), p.line2.color, p.line2.alpha);
        let tile = Rect::new(chip.x + ctx.px(12) as i32, chip.y + ctx.px(11) as i32, ctx.px(26), ctx.px(26));
        let ink: Color = match row.app.tone() {
            Tone::Ok => {
                draw::rounded_gradient(back, tile, ctx.px(9), p.ok, p.ok2, 255);
                Color::rgb(0xFF, 0xFF, 0xFF)
            }
            _ => {
                draw::rounded(back, tile, ctx.px(9), p.btn.color, p.btn.alpha);
                draw::rounded_stroke(back, tile, ctx.px(9), p.btnline.color, p.btnline.alpha);
                p.ink3
            }
        };
        let glyph = ctx.px(14);
        let icon = if row.app == App::Terminal { Icon::Prompt } else { row.app.icon() };
        glyphicon::draw(back, icon, tile.x + (tile.w as i32 - glyph as i32) / 2, tile.y + (tile.h as i32 - glyph as i32) / 2, glyph, ink, 255);
        let text_x = tile.right() + ctx.px(10) as i32;
        let room = (chip.right() - ctx.px(10) as i32 - text_x).max(0) as u32;
        let label = ctx.face(Role::Label);
        let top = chip.y + (chip.h as i32 - i32::from(label.line) - i32::from(mono.line)) / 2;
        paint::text_clipped(ctx, back, Role::Label, text_x, top, room, &row.title, p.ink2);
        paint::text_clipped(ctx, back, Role::MonoSmall, text_x, top + i32::from(label.line), room, &row.note, p.ink4);
    }
}

/// «задача», «задачи», «задач» — по числу.
fn tasks_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, rem) if rem != 11 => "задача",
        (2..=4, rem) if !(12..=14).contains(&rem) => "задачи",
        _ => "задач",
    }
}

/// Строки «Последнего»: свёрнутые окна, последнее свёрнутое первым.
#[must_use]
pub fn recent_rows(windows: &[(App, String, u64)]) -> Vec<Recent> {
    let mut sorted: Vec<&(App, String, u64)> = windows.iter().collect();
    sorted.sort_by(|a, b| b.2.cmp(&a.2));
    sorted
        .into_iter()
        .map(|(app, title, _)| Recent {
            app: *app,
            title: title.clone(),
            note: String::from(match app {
                App::Terminal => "оболочка",
                App::Settings => "параметры",
                App::Program(..) => "программа",
                _ => "окно",
            }),
        })
        .collect()
}
