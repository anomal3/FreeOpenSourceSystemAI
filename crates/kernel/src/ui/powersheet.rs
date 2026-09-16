// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Лист «Выключить телефон?» (макет `FreeOS-mobile`, экран 08).
//!
//! # Что это
//!
//! Кнопка питания в «Пуске» телефона открывает над доком лист: значок, вопрос,
//! пояснение, красная «Выключить» во всю ширину и под ней «Перезагрузить» и
//! «Отмена». На настольной машине тот же вопрос задаёт окно-диалог; на
//! телефоне окно во весь экран ради трёх кнопок — не ответ, а помеха.
//!
//! # Когда «Выключить» не нажимается
//!
//! Телефон на MediaTek погасить себя не умеет (см. `arch::aarch64::power`):
//! питание снимает контроллер PMIC, и порядок его команд не описан. Кнопка на
//! таком аппарате остаётся на месте, но приглушена и не нажимается, а
//! пояснение говорит почему и что делать вместо. Спрятать её значило бы
//! оставить человека искать выключение, которого нет.

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use super::tray::wrap_px;

/// Во что попало нажатие.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    PowerOff,
    Restart,
    Cancel,
    /// Внутри листа, мимо кнопок, — ничего не делать.
    Inside,
}

/// Числа макета, в его точках.
const PAD: u32 = 18;
const HEADER_H: u32 = 40;
const GAP: u32 = 14;
const NOTE_PAD: u32 = 12;
const BUTTON_H: u32 = 48;
const ROW_H: u32 = 44;
const BUTTON_GAP: u32 = 10;
/// Сколько лист отстоит от низа экрана: док и поле под ним (как у `tray`).
const ABOVE_BOTTOM: u32 = 100;

const NOTE_OFF: &str = "Открытые программы закроются. Тома сбросятся на диск и будут помечены чистыми — проверка при следующей загрузке не понадобится.";
const NOTE_NO_OFF: &str = "Погасить этот телефон системе нечем: питание снимает контроллер PMIC, а порядок его команд не описан. Держите кнопку питания или перезагрузите.";

pub struct Sheet {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    can_off: bool,
    off: Rect,
    restart: Rect,
    cancel: Rect,
}

fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

impl Sheet {
    /// Собрать лист над доком. `can_off` — умеет ли машина выключиться сама.
    #[must_use]
    pub fn open(screen_w: u32, screen_h: u32, scale: u32, can_off: bool) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        let inset = ctx.px(theme::M_INSET);
        let width = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let note = if can_off { NOTE_OFF } else { NOTE_NO_OFF };
        let caption = ctx.face(Role::Caption);
        let note_w = width.saturating_sub(ctx.px((PAD + NOTE_PAD) * 2));
        let lines = wrap_px(note, note_w, |text| caption.width(text)).len() as u32;
        let note_h = lines * u32::from(caption.line) + ctx.px(NOTE_PAD * 2);
        let height = ctx.px(PAD * 2 + HEADER_H + GAP * 2 + BUTTON_H + BUTTON_GAP + ROW_H) + note_h;
        let bottom = screen_h.checked_sub(ctx.px(ABOVE_BOTTOM))?;
        let top = bottom.checked_sub(height)?;
        let surface = Surface::new(width, height, bg())?;
        let mut sheet = Self {
            surface,
            rect: Rect::new(inset as i32, top as i32, width, height),
            scale,
            can_off,
            off: Rect::EMPTY,
            restart: Rect::EMPTY,
            cancel: Rect::EMPTY,
        };
        sheet.draw(note, note_h);
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
        if self.can_off && self.off.contains(x, y) {
            return Some(Hit::PowerOff);
        }
        if self.restart.contains(x, y) {
            return Some(Hit::Restart);
        }
        if self.cancel.contains(x, y) {
            return Some(Hit::Cancel);
        }
        Some(Hit::Inside)
    }

    fn draw(&mut self, note: &str, note_h: u32) {
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

        // Заголовок: красная плитка со значком, вопрос и строка под ним.
        let tile = Rect::new(pad, y + ctx.px(2) as i32, ctx.px(36), ctx.px(36));
        let tr = ctx.px(12);
        draw::rounded(&mut self.surface, tile, tr, p.badbg.color, p.badbg.alpha);
        draw::rounded_stroke(&mut self.surface, tile, tr, p.badline, 255);
        let glyph = ctx.px(16);
        glyphicon::draw(
            &mut self.surface,
            Icon::Power,
            tile.x + (tile.w as i32 - glyph as i32) / 2,
            tile.y + (tile.h as i32 - glyph as i32) / 2,
            glyph,
            p.bad_ink,
            255,
        );
        let text_x = tile.right() + ctx.px(12) as i32;
        let title = ctx.face(Role::Title);
        let small = ctx.face(Role::Caption);
        let lines_h = i32::from(title.line) + i32::from(small.line);
        let top = y + (ctx.px(HEADER_H) as i32 - lines_h) / 2;
        let question = if theme::is_mobile() { "Выключить телефон?" } else { "Выключить компьютер?" };
        paint::text(ctx, &mut self.surface, Role::Title, text_x, top, question, p.ink);
        let sub = if self.can_off { "Тома уйдут на диск" } else { "Перезагрузка работает" };
        paint::text(ctx, &mut self.surface, Role::Caption, text_x, top + i32::from(title.line), sub, p.ink4);
        y += ctx.px(HEADER_H + GAP) as i32;

        // Пояснение.
        let box_ = Rect::new(pad, y, inner_w, note_h);
        draw::rounded(&mut self.surface, box_, ctx.px(16), p.card.color, p.card.alpha);
        draw::rounded_stroke(&mut self.surface, box_, ctx.px(16), p.line2.color, p.line2.alpha);
        let area = box_.shrink(ctx.px(NOTE_PAD));
        let mut line_y = area.y;
        for line in wrap_px(note, area.w, |text| small.width(text)) {
            paint::text(ctx, &mut self.surface, Role::Caption, area.x, line_y, line, p.ink3);
            line_y += i32::from(small.line);
        }
        y += (note_h + ctx.px(GAP)) as i32;

        // «Выключить» — во всю ширину, красная; приглушённая, если нечем.
        let off = Rect::new(pad, y, inner_w, ctx.px(BUTTON_H));
        let br = ctx.px(16);
        let off_ink = if self.can_off {
            draw::rounded_gradient(&mut self.surface, off, br, p.bad, p.bad.mix(Color::rgb(0, 0, 0), 48), 255);
            Color::rgb(0xFF, 0xFF, 0xFF)
        } else {
            draw::rounded(&mut self.surface, off, br, p.ghost.color, p.ghost.alpha);
            draw::rounded_stroke(&mut self.surface, off, br, p.line2.color, p.line2.alpha);
            p.ink5
        };
        centered(ctx, &mut self.surface, off, "Выключить", off_ink, None);
        self.off = off;
        y += ctx.px(BUTTON_H + BUTTON_GAP) as i32;

        // «Перезагрузить» и «Отмена» — пополам.
        let gap = ctx.px(BUTTON_GAP);
        let half = inner_w.saturating_sub(gap) / 2;
        let restart = Rect::new(pad, y, half, ctx.px(ROW_H));
        let cancel = Rect::new(pad + (half + gap) as i32, y, inner_w - half - gap, ctx.px(ROW_H));
        for rect in [restart, cancel] {
            draw::rounded(&mut self.surface, rect, br, p.btn.color, p.btn.alpha);
            draw::rounded_stroke(&mut self.surface, rect, br, p.btnline.color, p.btnline.alpha);
        }
        centered(ctx, &mut self.surface, restart, "Перезагрузить", p.ink, Some(Icon::Update));
        centered(ctx, &mut self.surface, cancel, "Отмена", p.ink, None);
        self.restart = restart;
        self.cancel = cancel;
    }
}

/// Подпись кнопки по центру, со значком слева от неё.
fn centered(ctx: Ctx, surface: &mut Surface, rect: Rect, label: &str, ink: Color, icon: Option<Icon>) {
    let glyph = ctx.px(14);
    let gap = ctx.px(8);
    let text_w = ctx.face(Role::Label).width(label);
    let total = text_w + icon.map_or(0, |_| glyph + gap);
    let mut x = rect.x + (rect.w as i32 - total as i32) / 2;
    if let Some(icon) = icon {
        glyphicon::draw(surface, icon, x, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, ink, 255);
        x += (glyph + gap) as i32;
    }
    paint::text(ctx, surface, Role::Label, x, paint::baseline(ctx, Role::Label, rect), label, ink);
}
