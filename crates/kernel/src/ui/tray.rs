// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Лист «Свёрнутые программы» телефона (макет `FreeOS-mobile`, экран 09).
//!
//! # Что это
//!
//! Нажатие на стопку в доке открывает над ней список свёрнутых окон: значок и
//! цвет программы, имя, пояснение, сколько окно лежит свёрнутым, крестик
//! закрытия. Нажатие на строку разворачивает окно, «Развернуть все» — все.
//!
//! # Почему снимок, а не живой список
//!
//! Строки собираются в миг открытия и больше не меняются, а стол закрывает
//! лист при любом изменении окон. Живой список, перестраивающийся под пальцем,
//! означал бы, что строка, в которую целились, уехала за миг до нажатия и
//! нажатие досталось соседней — развернуть или закрыть не то окно.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use super::window::App;

/// Строка листа — то, что о свёрнутом окне известно в миг открытия.
pub struct Row {
    pub app: App,
    pub title: String,
    pub note: String,
    /// Сколько окно лежит свёрнутым, словами: «сейчас», «2 мин».
    pub age: String,
}

/// Во что попало нажатие.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Развернуть это окно.
    Restore(App),
    /// Закрыть это окно.
    Close(App),
    /// Развернуть все.
    RestoreAll,
    /// Мимо строк, но внутри листа — ничего не делать.
    Inside,
}

/// Числа макета, в его точках.
const PAD: u32 = 16;
const HEADER_H: u32 = 20;
const GAP: u32 = 14;
const ROW_H: u32 = 60;
const ROW_GAP: u32 = 8;
const NOTE_H: u32 = 56;
const HANDLE_H: u32 = 4;
/// Сколько лист отстоит от низа экрана: док и поле под ним.
const ABOVE_BOTTOM: u32 = 100;

/// Что сказано внизу листа — дословно из макета: это ответ на вопрос «а что
/// с программой, пока окно свёрнуто», который человек задаёт первым.
const NOTE: &str = "Свёрнутое окно не перерисовывается: задача остаётся в памяти, но композитор её не опрашивает.";

pub struct Sheet {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    rows: Vec<(Rect, Rect, App)>,
    restore_all: Rect,
}

fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

impl Sheet {
    /// Собрать лист над доком. `None` — строк нет или не хватило памяти.
    #[must_use]
    pub fn open(screen_w: u32, screen_h: u32, scale: u32, rows: &[Row]) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let ctx = Ctx::scaled(scale);
        let inset = ctx.px(theme::M_INSET);
        let width = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let count = rows.len() as u32;
        let height = ctx.px(
            PAD * 2 + HEADER_H + GAP + ROW_H * count + ROW_GAP * (count - 1) + GAP + NOTE_H + GAP + HANDLE_H,
        );
        let bottom = screen_h.checked_sub(ctx.px(ABOVE_BOTTOM))?;
        let top = bottom.checked_sub(height)?;
        let surface = Surface::new(width, height, bg())?;
        let mut sheet = Self {
            surface,
            rect: Rect::new(inset as i32, top as i32, width, height),
            scale,
            rows: Vec::new(),
            restore_all: Rect::EMPTY,
        };
        sheet.draw(rows);
        Some(sheet)
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Во что попала точка экрана. `None` — мимо листа.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        if self.restore_all.contains(x, y) {
            return Some(Hit::RestoreAll);
        }
        for (row, close, app) in &self.rows {
            // Крестик проверяется раньше строки: он лежит внутри неё.
            if close.contains(x, y) {
                return Some(Hit::Close(*app));
            }
            if row.contains(x, y) {
                return Some(Hit::Restore(*app));
            }
        }
        Some(Hit::Inside)
    }

    fn draw(&mut self, rows: &[Row]) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(30);
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);

        let pad = ctx.px(PAD) as i32;
        let inner_w = card.w.saturating_sub(ctx.px(PAD) * 2);
        let mut y = pad;

        // Заголовок: имя, счётчик, «Развернуть все».
        let header = Rect::new(pad + ctx.px(4) as i32, y, inner_w.saturating_sub(ctx.px(8)), ctx.px(HEADER_H));
        let title = "Свёрнутые программы";
        let title_w = paint::text(ctx, &mut self.surface, Role::Title, header.x, paint::baseline(ctx, Role::Title, header), title, p.ink);
        let count = alloc::format!("{}", rows.len());
        let mono = ctx.face(Role::MonoSmall);
        let pill_w = mono.width(&count) + ctx.px(16);
        let pill = Rect::new(header.x + (title_w + ctx.px(10)) as i32, header.y, pill_w, ctx.px(20));
        draw::rounded(&mut self.surface, pill, pill.h / 2, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(&mut self.surface, pill, pill.h / 2, p.btnline.color, p.btnline.alpha);
        paint::text(ctx, &mut self.surface, Role::MonoSmall, pill.x + ctx.px(8) as i32, paint::baseline(ctx, Role::MonoSmall, pill), &count, p.ink3);
        let all = "Развернуть все";
        let all_w = ctx.face(Role::Label).width(all);
        let all_x = header.right() - all_w as i32;
        paint::text(ctx, &mut self.surface, Role::Label, all_x, paint::baseline(ctx, Role::Label, header), all, p.acc_ink);
        // Попадание шире надписи: в неё целятся пальцем.
        self.restore_all = Rect::new(all_x - ctx.px(8) as i32, header.y - ctx.px(8) as i32, all_w + ctx.px(16), header.h + ctx.px(16));
        y += ctx.px(HEADER_H + GAP) as i32;

        // Строки. Первая — окно, свёрнутое последним, — подсвечена: его
        // скорее всего и ищут.
        self.rows.clear();
        for (index, row) in rows.iter().enumerate() {
            let rect = Rect::new(pad, y, inner_w, ctx.px(ROW_H));
            let r = ctx.px(18);
            if index == 0 {
                draw::horizontal_gradient(&mut self.surface, rect, r, ctx.flat(p.sel1), ctx.flat(p.sel2), 255);
                draw::rounded_stroke(&mut self.surface, rect, r, p.accedge, 255);
            } else {
                draw::rounded(&mut self.surface, rect, r, p.card.color, p.card.alpha);
                draw::rounded_stroke(&mut self.surface, rect, r, p.line2.color, p.line2.alpha);
            }

            let tile = Rect::new(rect.x + ctx.px(12) as i32, rect.y + ctx.px(12) as i32, ctx.px(36), ctx.px(36));
            let tr = ctx.px(12);
            let ink = match row.app.tone() {
                Tone::Ok => {
                    draw::rounded_gradient(&mut self.surface, tile, tr, p.ok, p.ok2, 255);
                    Color::rgb(0xFF, 0xFF, 0xFF)
                }
                Tone::Bad => {
                    draw::rounded(&mut self.surface, tile, tr, p.bad, 255);
                    Color::rgb(0xFF, 0xFF, 0xFF)
                }
                _ => {
                    draw::rounded(&mut self.surface, tile, tr, p.btn.color, p.btn.alpha);
                    draw::rounded_stroke(&mut self.surface, tile, tr, p.btnline.color, p.btnline.alpha);
                    p.ink3
                }
            };
            let glyph = ctx.px(15);
            let icon = if row.app == App::Terminal { Icon::Prompt } else { row.app.icon() };
            glyphicon::draw(
                &mut self.surface,
                icon,
                tile.x + (tile.w as i32 - glyph as i32) / 2,
                tile.y + (tile.h as i32 - glyph as i32) / 2,
                glyph,
                ink,
                255,
            );

            // Справа: крестик и возраст.
            let close = Rect::new(rect.right() - ctx.px(12 + 28) as i32, rect.y + ctx.px(16) as i32, ctx.px(28), ctx.px(28));
            let cr = ctx.px(10);
            draw::rounded(&mut self.surface, close, cr, p.badbg.color, p.badbg.alpha);
            draw::rounded_stroke(&mut self.surface, close, cr, p.badline, 255);
            let cross = ctx.px(12);
            glyphicon::draw(
                &mut self.surface,
                Icon::Close,
                close.x + (close.w as i32 - cross as i32) / 2,
                close.y + (close.h as i32 - cross as i32) / 2,
                cross,
                p.bad_ink,
                255,
            );
            let age_w = mono.width(&row.age);
            let age_x = close.x - ctx.px(9) as i32 - age_w as i32;
            paint::text(ctx, &mut self.surface, Role::MonoSmall, age_x, paint::baseline(ctx, Role::MonoSmall, rect), &row.age, p.ink4);

            // Имя и пояснение между плиткой и возрастом.
            let text_x = tile.right() + ctx.px(12) as i32;
            let room = (age_x - ctx.px(9) as i32 - text_x).max(0) as u32;
            let title_face = ctx.face(Role::Title);
            let lines_h = i32::from(title_face.line) + i32::from(mono.line);
            let top = rect.y + (rect.h as i32 - lines_h) / 2;
            paint::text_clipped(ctx, &mut self.surface, Role::Title, text_x, top, room, &row.title, p.ink);
            paint::text_clipped(ctx, &mut self.surface, Role::MonoSmall, text_x, top + i32::from(title_face.line), room, &row.note, p.ink4);

            // Попадание в строку — вся строка, в крестик — чуть шире самого
            // крестика: промах мимо него на пару точек иначе разворачивал бы
            // окно, которое хотели закрыть.
            let close_hit = Rect::new(close.x - ctx.px(6) as i32, rect.y, close.w + ctx.px(12), rect.h);
            self.rows.push((rect, close_hit, row.app));
            y += ctx.px(ROW_H + ROW_GAP) as i32;
        }
        y += ctx.px(GAP - ROW_GAP) as i32;

        // Пояснение.
        let note = Rect::new(pad, y, inner_w, ctx.px(NOTE_H));
        draw::rounded(&mut self.surface, note, ctx.px(16), p.card.color, p.card.alpha);
        draw::rounded_stroke(&mut self.surface, note, ctx.px(16), p.line2.color, p.line2.alpha);
        let area = note.shrink(ctx.px(12));
        let caption = ctx.face(Role::Caption);
        let mut line_y = area.y;
        for line in wrap_px(NOTE, area.w, |text| caption.width(text)) {
            paint::text(ctx, &mut self.surface, Role::Caption, area.x, line_y, line, p.ink4);
            line_y += i32::from(caption.line);
        }
        y += ctx.px(NOTE_H + GAP) as i32;

        // Ручка.
        let handle = Rect::new(card.w as i32 / 2 - ctx.px(22) as i32, y, ctx.px(44), ctx.px(HANDLE_H));
        draw::rounded(&mut self.surface, handle, handle.h / 2, p.ink6, 180);
    }
}

/// Сколько окно лежит свёрнутым, словами.
#[must_use]
pub fn age_text(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes == 0 {
        String::from("сейчас")
    } else if minutes < 60 {
        alloc::format!("{minutes} мин")
    } else {
        alloc::format!("{} ч", minutes / 60)
    }
}

/// Перенос по словам по ширине в точках: сколько слов влезает в `width`.
///
/// Свой, а не `widget::wrap`: тот считает знаки, а шрифт у нас
/// пропорциональный, и строка из «ш» вдвое шире строки из «і».
pub fn wrap_px(text: &str, width: u32, measure: impl Fn(&str) -> u32) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut last_fit = 0;
    for (at, _) in text.match_indices(' ').chain(core::iter::once((text.len(), ""))) {
        if measure(&text[start..at]) > width && last_fit > start {
            lines.push(&text[start..last_fit]);
            start = last_fit + 1;
        }
        last_fit = at;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}
